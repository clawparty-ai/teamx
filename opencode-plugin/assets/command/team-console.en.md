---
description: teamx open the member web console (teamx console, opens the browser)
agent: teamx.en
---

User wants to open the Teamx Console (member local read-only web console). Call teamx_console (port defaults to 9528; if already running it just re-opens the existing URL).

After a successful start, show the user the URL (`https://127.0.0.1:9528/?token=…`) and note:
- The console is read-only and shows the current workspace's (.teamx) team info — the same data as the teamx CLI;
- Before using the Cloud panel, run `teamx tunnel forward <name> --local-port <port>` to map a cloud node locally, then add the `http://127.0.0.1:<port>` node to embed it in an iframe.
