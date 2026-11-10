---
title: chat connectors
section: use
order: 5
---

Chat apps that support custom connectors can use Sphinx through the same MCP server, served over HTTP.

## serve it

```sh
SPHINX_MCP_TOKEN=<32+ letters and digits> \
SPHINX_MCP_HOSTS=your-host.example \
sphinx-mcp --http 127.0.0.1:8790
```

The tools are then at `https://your-host.example/mcp/<token>`. Put a tunnel or proxy with HTTPS in front of it.

| Variable | Effect |
|---|---|
| `SPHINX_MCP_TOKEN` | serves at `/mcp/<token>`, so only someone holding the full URL can use it |
| `SPHINX_MCP_HOSTS` | host names to accept besides loopback, such as your tunnel |

## add it to the chat app

Add a custom connector and paste the full URL. The assistant can then list jobs, read tasks, check code, submit, and post jobs.

> Every caller signs as the one configured account, inside one spending cap. Use one connector per person. A public connector shared by many people needs per-user sign-in.
