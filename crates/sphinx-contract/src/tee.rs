//! The enclave proof, checked on chain.
//!
//! Every grading run happens in an OutLayer Intel TDX enclave, and OutLayer
//! publishes one attestation per run: the run's fields (build, input and
//! output hashes, caller, the buyer's stored tests) and a TDX quote whose
//! report data commits to the SHA-256 of those fields. `confirm_run` checks
//! that an attestation describes a run of this job's frozen grader build,
//! started by the grader's caller, reading this job's hidden tests, and that
//! its output is exactly the signed result about to be recorded. It then asks
//! NEAR's DCAP verifier contract to check Intel's signature on the quote, and
//! in the callback checks the report data, the TCB status and that the
//! worker image is on OutLayer's approved list. Only then is the result
//! confirmed, and `record_score` / `record_suite_check` accept a signed result
//! only once its run is confirmed.
//!
//! The approved list is read from OutLayer's registry contract by
//! `sync_measurements`, which anyone may call; the owner has no say in it.
//!
//! The verifier's return types below mirror near/mpc's
//! `tee-verifier-interface` (borsh). The same mirrors appear in the MIT
//! licensed Antibody contract (github.com/Himess/antibody), which this module
//! follows for the call shape.

use near_sdk::json_types::{Base64VecU8, U64};
use near_sdk::serde::Deserialize;
use near_sdk::serde_json;
use near_sdk::{env, near, AccountId, Gas, NearToken, Promise, PromiseError};
use sphinx_protocol::{Code, SphinxError};

use crate::errors::{ensure, Result as Outcome};
use crate::events::SphinxEvent;
use crate::{Sphinx, SphinxExt, StorageKey};

/// NEAR's DCAP `verify_quote` burns about 176 Tgas.
const VERIFY_GAS: Gas = Gas::from_tgas(200);
const CONFIRMED_GAS: Gas = Gas::from_tgas(20);
const REGISTRY_GAS: Gas = Gas::from_tgas(20);
const SYNCED_GAS: Gas = Gas::from_tgas(30);
/// Most worker images kept from OutLayer's list.
const MAX_IMAGES: usize = 400;
/// Largest grader output read back (the signed result is far smaller).
const MAX_OUTPUT_BYTES: usize = 8 * 1024;

/// Where the enclave proof is checked, fixed at initialisation.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnclaveCheck {
    /// NEAR's DCAP verifier contract (`tee-verifier-2026-08-04.near` on mainnet).
    pub quote_verifier: AccountId,
    /// OutLayer's registry of approved worker images (`worker.outlayer.near`).
    pub measurements_from: AccountId,
    /// The account that starts grading runs (the relayer).
    pub grader_caller: AccountId,
}

/// The two fields of a grader run's output the contract reads; a typed
/// struct, not a generic JSON value, keeps float formatting out of the build.
#[derive(Debug, Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct RunOutput {
    ok: Option<bool>,
    message: Option<String>,
}

/// One entry of OutLayer's approved image list; other fields are ignored.
#[near(serializers = [json])]
#[derive(Debug)]
pub struct ListedImage {
    mrtd: Option<String>,
    rtmr0: Option<String>,
    rtmr1: Option<String>,
    rtmr2: Option<String>,
    rtmr3: Option<String>,
}

/// OutLayer's per-run attestation, as its API returns it.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct RunAttestation {
    pub task_type: String,
    pub task_id: i64,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub commit_hash: Option<String>,
    #[serde(default)]
    pub build_target: Option<String>,
    #[serde(default)]
    pub wasm_hash: Option<String>,
    #[serde(default)]
    pub input_hash: Option<String>,
    pub output_hash: String,
    #[serde(default)]
    pub block_height: Option<i64>,
    #[serde(default)]
    pub caller_account_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub secrets_ref: Option<String>,
    pub timestamp: i64,
    #[serde(default)]
    pub attached_usd: Option<String>,
    #[serde(default)]
    pub executed_wasm_sha256: Option<String>,
}

impl RunAttestation {
    /// OutLayer's task hash: SHA-256 over the fields in this order, integers
    /// as 8-byte little-endian, absent or empty text skipped. The enclave
    /// puts it in the first 32 bytes of the quote's report data.
    pub fn task_hash(&self) -> Vec<u8> {
        let mut m: Vec<u8> = Vec::new();
        let text = |m: &mut Vec<u8>, v: &Option<String>| {
            if let Some(s) = v.as_deref().filter(|s| !s.is_empty()) {
                m.extend_from_slice(s.as_bytes());
            }
        };
        m.extend_from_slice(self.task_type.as_bytes());
        m.extend_from_slice(&self.task_id.to_le_bytes());
        for v in [
            &self.repo_url,
            &self.commit_hash,
            &self.build_target,
            &self.wasm_hash,
            &self.input_hash,
        ] {
            text(&mut m, v);
        }
        m.extend_from_slice(self.output_hash.as_bytes());
        if let Some(h) = self.block_height {
            m.extend_from_slice(&h.to_le_bytes());
        }
        for v in [&self.caller_account_id, &self.project_id, &self.secrets_ref] {
            text(&mut m, v);
        }
        m.extend_from_slice(&self.timestamp.to_le_bytes());
        text(&mut m, &self.attached_usd);
        env::sha256(&m)
    }
}

/// One approved worker image, lowercase hex.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measurements {
    pub mrtd: String,
    pub rtmr0: String,
    pub rtmr1: String,
    pub rtmr2: String,
    pub rtmr3: String,
}

// Borsh mirrors of near/mpc `tee-verifier-interface`.

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum VerificationResult {
    Verified(VerifiedReport),
    Rejected(VerifierError),
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub enum VerifierError {
    DcapVerification(String),
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct VerifiedReport {
    pub status: String,
    pub advisory_ids: Vec<String>,
    pub report: Report,
    pub ppid: Vec<u8>,
    pub qe_status: TcbStatusWithAdvisory,
    pub platform_status: TcbStatusWithAdvisory,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Report {
    SgxEnclave(EnclaveReport),
    TD10(TdReport10),
    TD15(TdReport15),
    TD15Ex(TdReport15Ex),
}

impl Report {
    fn td10(&self) -> Option<&TdReport10> {
        match self {
            Report::TD10(r) => Some(r),
            Report::TD15(r) => Some(&r.base),
            Report::TD15Ex(r) => Some(&r.base.base),
            Report::SgxEnclave(_) => None,
        }
    }
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct TdReport10 {
    pub tee_tcb_svn: [u8; 16],
    pub mr_seam: [u8; 48],
    pub mr_signer_seam: [u8; 48],
    pub seam_attributes: [u8; 8],
    pub td_attributes: [u8; 8],
    pub xfam: [u8; 8],
    pub mr_td: [u8; 48],
    pub mr_config_id: [u8; 48],
    pub mr_owner: [u8; 48],
    pub mr_owner_config: [u8; 48],
    pub rt_mr0: [u8; 48],
    pub rt_mr1: [u8; 48],
    pub rt_mr2: [u8; 48],
    pub rt_mr3: [u8; 48],
    pub report_data: [u8; 64],
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct TdReport15 {
    pub base: TdReport10,
    pub tee_tcb_svn2: [u8; 16],
    pub mr_service_td: [u8; 48],
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct TdReport15Ex {
    pub base: TdReport15,
    pub vmid: u8,
    pub td_id: [u8; 32],
    pub dev_info: [u8; 48],
    pub init_service_td_hash: [u8; 48],
    pub init_service_td_attributes: [u8; 8],
    pub init_cpu_svn: [u8; 16],
    pub init_tee_tcb_svn: [u8; 16],
    pub init_tee_fmspc: [u8; 12],
    pub cur_service_td_hash: [u8; 48],
    pub cur_service_td_attributes: [u8; 8],
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct EnclaveReport {
    pub cpu_svn: [u8; 16],
    pub misc_select: u32,
    pub reserved1: [u8; 28],
    pub attributes: [u8; 16],
    pub mr_enclave: [u8; 32],
    pub reserved2: [u8; 32],
    pub mr_signer: [u8; 32],
    pub reserved3: [u8; 96],
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub reserved4: [u8; 60],
    pub report_data: [u8; 64],
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub enum TcbStatus {
    UpToDate,
    OutOfDateConfigurationNeeded,
    OutOfDate,
    ConfigurationAndSWHardeningNeeded,
    ConfigurationNeeded,
    SWHardeningNeeded,
    Revoked,
    TDRelaunchAdvised,
    TDRelaunchAdvisedConfigurationNeeded,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct TcbStatusWithAdvisory {
    pub status: TcbStatus,
    pub advisory_ids: Vec<String>,
}

impl Measurements {
    /// What the contract stores per approved image: SHA-256 over the five
    /// measurements' raw bytes, so the list costs 32 bytes an image.
    #[must_use]
    pub fn digest(&self) -> Option<[u8; 32]> {
        let mut raw = Vec::with_capacity(240);
        for h in [&self.mrtd, &self.rtmr0, &self.rtmr1, &self.rtmr2, &self.rtmr3] {
            let b = hex::decode(h).ok().filter(|b| b.len() == 48)?;
            raw.extend_from_slice(&b);
        }
        Some(env::sha256_array(&raw))
    }
}

fn measurements_of(td: &TdReport10) -> Measurements {
    Measurements {
        mrtd: hex::encode(td.mr_td),
        rtmr0: hex::encode(td.rt_mr0),
        rtmr1: hex::encode(td.rt_mr1),
        rtmr2: hex::encode(td.rt_mr2),
        rtmr3: hex::encode(td.rt_mr3),
    }
}

/// The confirmed-runs key: (job, submission), with no submission for the
/// job's suite check.
fn confirmed_key(job_id: u64, sub_id: Option<u32>) -> Vec<u8> {
    let mut k = near_sdk::IntoStorageKey::into_storage_key(StorageKey::ConfirmedRuns);
    k.extend_from_slice(&job_id.to_le_bytes());
    match sub_id {
        Some(s) => {
            k.push(1);
            k.extend_from_slice(&s.to_le_bytes());
        }
        None => k.push(0),
    }
    k
}

fn refused(why: impl Into<String>) -> SphinxError {
    SphinxError::new(Code::EnclaveProofRefused, why)
}

#[near]
impl Sphinx {
    /// Confirms one grading run of `job_id` (submission `sub_id`, or the
    /// suite check when `sub_id` is absent) from OutLayer's attestation, the
    /// run's raw `output`, the TDX `quote` and Intel's `collateral` (borsh,
    /// as NEAR's DCAP verifier takes it). Anyone may call it; the result is
    /// recorded only if every check passes. Attach 250 Tgas or more.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_ENCLAVE_PROOF_REFUSED` when the attestation does not
    ///   describe this job's run, or the contract has no enclave check
    /// - `SPHINX_E_JOB_NOT_FOUND`
    #[handle_result(aliased)]
    pub fn confirm_run(
        &mut self,
        job_id: U64,
        sub_id: Option<u32>,
        attestation: RunAttestation,
        output: Base64VecU8,
        quote: Base64VecU8,
        collateral: Base64VecU8,
    ) -> Outcome<Promise> {
        let check =
            self.enclave.clone().ok_or_else(|| refused("this contract has no enclave check"))?;
        let job = self.job(job_id.0)?;
        let commit = job
            .commit
            .as_ref()
            .ok_or_else(|| refused(format!("job {} has no committed suite", job_id.0)))?;
        let a = &attestation;
        ensure!(a.task_type == "execute", refused("the attestation is not of an execution"));
        ensure!(
            a.build_target.as_deref() == Some("wasm32-wasip2"),
            refused("the attested build target is not wasm32-wasip2")
        );
        let build = commit.grader.wasm_sha256.to_string();
        ensure!(
            a.wasm_hash.as_deref() == Some(build.as_str())
                && a.executed_wasm_sha256.as_deref().is_none_or(|e| e == build),
            refused(format!("the attested build is not this job's grader build {build}"))
        );
        ensure!(
            a.caller_account_id.as_deref() == Some(check.grader_caller.as_str()),
            refused(format!(
                "the run was not started by the grader's caller {}",
                check.grader_caller
            ))
        );
        let tests = format!("{}/{}", job.buyer, sphinx_protocol::hidden::profile_for_job(job_id.0));
        ensure!(
            a.secrets_ref.as_deref() == Some(tests.as_str()),
            refused(format!("the run did not read this job's hidden tests {tests}"))
        );
        ensure!(
            a.repo_url.is_none() && a.commit_hash.is_none() && a.project_id.is_none(),
            refused("the attestation is not of a build run by its hash")
        );
        ensure!(
            output.0.len() <= MAX_OUTPUT_BYTES,
            refused("the output is larger than a signed result")
        );
        ensure!(
            hex::encode(env::sha256(&output.0)) == a.output_hash,
            refused("the output is not the attested run's output")
        );
        let out: RunOutput =
            serde_json::from_slice(&output.0).map_err(|_| refused("the output is not JSON"))?;
        let message = out
            .message
            .filter(|_| out.ok == Some(true))
            .ok_or_else(|| refused("the output holds no signed result"))?;
        let report_data = hex::encode(a.task_hash());
        Ok(Promise::new(check.quote_verifier)
            .function_call(
                "verify_quote".to_owned(),
                verifier_args(&quote.0, &collateral.0),
                NearToken::from_yoctonear(0),
                VERIFY_GAS,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(CONFIRMED_GAS)
                    .on_run_verified(job_id, sub_id, report_data, message),
            ))
    }

    /// Records a run as confirmed once NEAR's verifier accepted its quote,
    /// the quote commits to the attestation, the platform is up to date and
    /// the worker image is on OutLayer's approved list.
    #[private]
    pub fn on_run_verified(
        &mut self,
        job_id: U64,
        sub_id: Option<u32>,
        report_data: String,
        message: String,
        #[callback_result]
        #[serializer(borsh)]
        result: Result<VerificationResult, PromiseError>,
    ) -> bool {
        let refuse = |why: String| {
            SphinxEvent::RunRefused { job_id, sub_id, reason: why }.emit();
            false
        };
        let v = match result {
            Ok(VerificationResult::Verified(v)) => v,
            Ok(VerificationResult::Rejected(VerifierError::DcapVerification(e))) => {
                return refuse(format!("Intel's signature on the quote was refused: {e}"))
            }
            Err(_) => return refuse("the quote verifier could not be reached".into()),
        };
        let Some(td) = v.report.td10() else {
            return refuse("the quote is not a TDX report".into());
        };
        if hex::encode(&td.report_data[..32]) != report_data || td.report_data[32..] != [0u8; 32] {
            return refuse("the quote does not commit to this run's attestation".into());
        }
        if v.status != "UpToDate" {
            return refuse(format!("the platform's TCB status is {}, not UpToDate", v.status));
        }
        if !measurements_of(td).digest().is_some_and(|d| self.approved_images.contains(&d)) {
            return refuse("the worker image is not on OutLayer's approved list (run sync_measurements if it was added since)".into());
        }
        env::storage_write(&confirmed_key(job_id.0, sub_id), message.as_bytes());
        SphinxEvent::RunConfirmed { job_id, sub_id }.emit();
        true
    }

    /// Reads OutLayer's approved worker images from its registry contract.
    /// Anyone may call it.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_ENCLAVE_PROOF_REFUSED` when the contract has no enclave check
    #[handle_result(aliased)]
    pub fn sync_measurements(&mut self) -> Outcome<Promise> {
        let check =
            self.enclave.clone().ok_or_else(|| refused("this contract has no enclave check"))?;
        Ok(Promise::new(check.measurements_from)
            .function_call(
                "get_approved_measurements".to_owned(),
                b"{}".to_vec(),
                NearToken::from_yoctonear(0),
                REGISTRY_GAS,
            )
            .then(
                Self::ext(env::current_account_id()).with_static_gas(SYNCED_GAS).on_measurements(),
            ))
    }

    /// Stores the list `sync_measurements` read.
    #[private]
    pub fn on_measurements(
        &mut self,
        #[callback_result] list: Result<Vec<ListedImage>, PromiseError>,
    ) -> u32 {
        let Ok(list) = list else { return 0 };
        let field = |v: &Option<String>| v.as_deref().map(str::to_ascii_lowercase);
        let images: Vec<[u8; 32]> = list
            .iter()
            .filter_map(|v| {
                Measurements {
                    mrtd: field(&v.mrtd)?,
                    rtmr0: field(&v.rtmr0)?,
                    rtmr1: field(&v.rtmr1)?,
                    rtmr2: field(&v.rtmr2)?,
                    rtmr3: field(&v.rtmr3)?,
                }
                .digest()
            })
            .take(MAX_IMAGES)
            .collect();
        let n = u32::try_from(images.len()).unwrap_or(u32::MAX);
        if n > 0 {
            self.approved_images = images;
            SphinxEvent::MeasurementsSynced { images: n }.emit();
        }
        n
    }

    /// The approved worker images the enclave check accepts, each as the
    /// SHA-256 of its five measurements (see `Measurements::digest`).
    pub fn get_approved_images(&self) -> Vec<String> {
        self.approved_images.iter().map(hex::encode).collect()
    }

    /// Whether the run of `job_id` (submission `sub_id`, or the suite check)
    /// is confirmed and waiting for its signed result.
    pub fn is_run_confirmed(&self, job_id: U64, sub_id: Option<u32>) -> bool {
        env::storage_has_key(&confirmed_key(job_id.0, sub_id))
    }
}

impl Sphinx {
    /// With an enclave check, a signed result counts only when its run was
    /// confirmed and the run's output signed exactly `message`. The record
    /// is removed once used.
    pub(crate) fn take_confirmed_run(
        &mut self,
        job_id: u64,
        sub_id: Option<u32>,
        message: &str,
    ) -> Outcome<()> {
        if self.enclave.is_none() {
            return Ok(());
        }
        let key = confirmed_key(job_id, sub_id);
        let stored = env::storage_read(&key).ok_or_else(|| {
            SphinxError::new(
                Code::EnclaveRunNotConfirmed,
                format!("the grading run of job {job_id} is not confirmed yet; call confirm_run with its attestation first"),
            )
            .ctx("job_id", job_id)
        })?;
        ensure!(
            stored == message.as_bytes(),
            SphinxError::new(
                Code::EnclaveRunNotConfirmed,
                "the confirmed run signed a different result"
            )
            .ctx("job_id", job_id)
        );
        env::storage_remove(&key);
        Ok(())
    }
}

/// `verify_quote` takes raw arguments: the quote's length (u32, little
/// endian), the quote, then the borsh collateral.
fn verifier_args(quote: &[u8], collateral: &[u8]) -> Vec<u8> {
    let mut args = Vec::with_capacity(quote.len() + collateral.len() + 4);
    args.extend_from_slice(&u32::try_from(quote.len()).unwrap_or(u32::MAX).to_le_bytes());
    args.extend_from_slice(quote);
    args.extend_from_slice(collateral);
    args
}

#[cfg(test)]
pub(crate) fn test_report(report_data: [u8; 64], image: &Measurements) -> VerificationResult {
    let h48 = |s: &str| -> [u8; 48] { hex::decode(s).unwrap().try_into().unwrap() };
    let tcb = TcbStatusWithAdvisory { status: TcbStatus::UpToDate, advisory_ids: vec![] };
    VerificationResult::Verified(VerifiedReport {
        status: "UpToDate".into(),
        advisory_ids: vec![],
        report: Report::TD10(TdReport10 {
            tee_tcb_svn: [0; 16],
            mr_seam: [0; 48],
            mr_signer_seam: [0; 48],
            seam_attributes: [0; 8],
            td_attributes: [0; 8],
            xfam: [0; 8],
            mr_td: h48(&image.mrtd),
            mr_config_id: [0; 48],
            mr_owner: [0; 48],
            mr_owner_config: [0; 48],
            rt_mr0: h48(&image.rtmr0),
            rt_mr1: h48(&image.rtmr1),
            rt_mr2: h48(&image.rtmr2),
            rt_mr3: h48(&image.rtmr3),
            report_data,
        }),
        ppid: vec![],
        qe_status: tcb.clone(),
        platform_status: tcb,
    })
}
