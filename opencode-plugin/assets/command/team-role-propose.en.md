---
description: teamx propose custom role (member self-service, owner approval)
agent: teamx
---

User wants to propose a custom role. Parameters: $ARGUMENTS (format: <key> <label> [description]). Call teamx_role_propose (role=key, label=label, description=description). The key must not conflict with built-in roles (owner/observer/supervisor/contributor/subtask-implementer/reviewer) and must not be a duplicate. After proposing, prompt the owner to approve with `/team role approve <key>`.
