# team-invite Design v2 (mTLS + Invitation Letter)

> Status: **I0/I1/I2/I3/I4 implemented** (PKI + enforced-mTLS serve + `team invite`/`team import` + certificate identity wired into RPC + revocation enforcement/active disconnect + plugin integration + cross-network LAN verification)
> Related: `docs/03-design-network.md` (network mode), `docs/01-design-v1-spec.md`
> Core upgrade: upgrades "guessable token / approval-free invite codes" to **mTLS certificate authorization**; the invite code becomes an **Invitation Letter** (an all-in-one package of certificates + address + role).

---

## 1. Goals

1. **mTLS mutual authentication**: teamx serve **enforces mTLS** — the server verifies member certificates, members verify the server certificate; two-way trust (no plaintext downgrade).
2. **Invitation Letter**: the owner generates a self-contained "invitation letter" package containing:
   - A **client certificate** signed by the owner (for mTLS connection)
   - The **server address** (`wss://<owner-ip>:5781`)
   - The invited member's **job role + job description**
3. **One-click onboarding**: the member imports the letter → establishes the mTLS connection → automatically receives the role → enters working mode.
4. **Owner approval still required**: a certificate is not approval-free — even holding a valid certificate, a member must still be approved by the owner before formally working (**guards against accidental certificate leakage**; the certificate means "can connect", approve means "can work").
5. **Certificate validity**: default **3650 days** (10 years).

---

## 2. User Flow

```
┌──────────── owner ────────────┐      ┌────────── member ──────────┐
│ 1. /team-serve start          │      │ 4. /team-import <letter>   │
│    (generate CA + server cert) │      │    (import invitation letter)│
│        │                       │      │    │                       │
│        ▼                       │      │    ▼                       │
│ 2. /team-invite "Test engineer: │      │ 5. Plugin uses the client  │
│    run tests, report results"   │      │    certificate from the    │
│        │                       │      │    letter for mTLS         │
│        ▼                       │      │    │                       │
│ 3. Generate invitation letter  │      │ 6. Submit join (pending)   │
│    (cert+address+role) → send  │      │    + auto-attached role    │
│        │                       │      │    request                 │
│        ▼                       │      │    ▼                       │
│ 7. Owner approve → member       │◄─────│ 7. Enter working mode     │
│    active (approve required,    │      │    + wait for tasks and    │
│    guards against cert leakage) │      │    auto-execute            │
└────────────────────────────────┘      └────────────────────────────┘
```

---

## 3. mTLS / PKI Architecture

### 3.1 Trust Model

```
                    ┌────────────────────────────┐
                    │  teamx CA (owner private)    │
                    │  self-signed root ca.crt / ca.key │
                    └──────┬─────────────┬───────┘
                           │ signs        │ signs
                    ┌──────▼─────┐  ┌──────▼──────┐
                    │ server.crt  │  │ member.crt  │  ← carried in invitation letter
                    │ server.key  │  │ member.key  │     (issued per member)
                    │ (held by serve) │  └─────────────┘
                    └─────────────┘
```

- **CA**: `~/.teamx/ca/` (owner private), auto-generated at `teamx serve start --mtls` if absent.
- **Server certificate**: signed by the `ca` for the server (CN=teamx-server, SAN=IP/DNS).
- **Member certificates**: one issued per `team-invite`, bound to `member_id + role_key` (role carried in CN or SAN).

### 3.2 Authentication Flow

```
member plugin ──TLS client hello──► teamx serve
    │ presents member.crt + member.key      │ verify: was member.crt issued by the teamx CA?
    │                                  │ parse CN/SAN → member_id + role
    ◄── server presents server.crt ────│ verify: was server.crt issued by the teamx CA?
    │ (member side checks CA fingerprint)  │ (mTLS both ways)
    │                                  │
    └──── mTLS established, identity = certificate identity ────┘
                │
                ▼
    Member join still needs owner approve (guards against cert leakage)
    Certificate = "can connect"; approve = "can work"
```

- No more `Authorization: Bearer <token>` — **identity authentication is completed at the TLS layer**.
- Identity source: `member_id` and `role_key` parsed from certificate CN/SAN (server side looks them up in the `invitations` table).
- **Approve remains mandatory**: the certificate allows establishing the connection and submitting the join request; only after owner approve does the member enter `active` working mode (so an accidentally leaked certificate is not immediately usable).

---

## 4. Invitation Letter Package

### 4.1 Format (self-contained JSON / PEM bundle)

```jsonc
{
  "teamx_invitation": {
    "version": 1,
    "invitation_id": "uuid",
    "team": { "id": "…", "name": "Acceptance Test Group" },
    "server": { "url": "wss://192.168.1.5:5781", "ca_fingerprint": "sha256:abcd…" },
    "member": { "name_hint": "" },
    "role": { "key": "tester", "label": "Test Engineer",
              "description": "Writes and executes test cases, reports results and defects." },
    "issued_at": "…", "expires_at": null
  },
  "certificates": {
    "ca_cert":     "-----BEGIN CERTIFICATE-----…",  // used to verify the server
    "client_cert": "-----BEGIN CERTIFICATE-----…",  // member identity
    "client_key":  "-----BEGIN PRIVATE KEY-----…"   // member private key (only in the letter, never stored server-side)
  }
}
```

> **Private-key safety**: client_key exists only inside the letter; the server/CA side never retains member private keys. The letter's transport channel is the owner's choice (secure channel / offline copy / encrypted transfer).

### 4.2 Convenient Distribution

- CLI output is a **single-line base64** string (`teamx-inv:v1:…`) for easy copy/paste; alternatively `--file letter.json` writes it to disk.
- On the member side, `teamx team import <letter>` (or base64) → the plugin unpacks it, stores it at `~/.teamx/letters/<invitation_id>.json` (0600), and establishes the connection.

---

## 5. New Commands

| Command | Tool | Description | Permission |
|---|---|---|---|
| `teamx serve start --mtls` | teamx_serve_start | Generate CA+server certs and start the service with mTLS | owner |
| `teamx team invite "<role>: <desc>"` | teamx_team_invite | Issue a member certificate + generate an invitation letter | owner |
| `teamx team invite list` | teamx_team_invite | List issued, unused invitation letters | owner |
| `teamx team invite revoke <id>` | teamx_team_invite | Revoke an invitation letter (update the revocation list) | owner |
| `teamx team import <letter>` | teamx_team_import | Import an invitation letter, establish the mTLS connection | member |
| `teamx team join` (token-free) | teamx_join (extended) | After import, the plugin joins automatically | member |

### 5.1 Certificate Revocation

- Add a `cert_revocations` table (or reuse `invitations.revoked_at`): the server **checks the revocation list during the TLS handshake**, rejecting immediately after revoke.
- Simple approach: add `revoked_at` to the `invitations` table; the server loads a CN→status map of issued invitations and validates at handshake.

---

## 6. DB v6 Migration

```sql
-- invitation letters (with certificate mapping)
CREATE TABLE IF NOT EXISTS invitations (
  id            TEXT PRIMARY KEY,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  role_key      TEXT NOT NULL,
  role_label    TEXT,
  role_desc     TEXT,
  cert_serial   TEXT,                 -- member certificate serial (for revocation)
  cert_cn       TEXT,                 -- member certificate CN (member_id)
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  used_by       TEXT,
  used_at       TEXT,
  revoked_at    TEXT
);
```

---

## 7. Plugin-Side (client.ts) Changes

```ts
// connection config comes from the invitation letter (or serve start's return)
type MtlsConfig = {
  serverUrl: string          // wss://<owner-ip>:5781
  caCert: string             // PEM
  clientCert: string         // PEM
  clientKey: string          // PEM
}
// Bun.fetch supports tls: { cert, key, ca } → establish mTLS HTTP/WS
```

- `TEAMX_SERVER_URL` remains the default target; when a letter exists, use the letter's serverUrl + mTLS.
- `runRpc` / WS connections use mTLS client certificates; identity is parsed from the certificate — no session/token transmitted.

---

## 8. Security Analysis

| Threat | Mitigated by mTLS + letter |
|---|---|
| Impersonating a member | Requires holding a valid member certificate signed by the CA |
| Replay/reuse of invitations | Letter is single-use (`used_by`) + certificate revocable |
| Man-in-the-middle | Server certificate CA-signed; members verify the CA fingerprint |
| Token leakage | No tokens; private key kept only on the member side |
| Private-key leakage | That invitation's certificate can be revoked |

**Boundaries**:
- V1 still has no real authentication (local CLI mode unchanged); mTLS applies only to network-mode connections.
- Security of the letter's transport channel is the owner's responsibility (noted in design docs).

---

## 9. Milestones

| Phase | Content | Acceptance |
|---|---|---|
| I0 | Rust PKI: CA generation, server/member certificate issuance, **enforced-mTLS** serve (rustls); certs default 3650d | ✅ done |
| I1 | `team invite` issues letters; `team import` imports and connects via mTLS; RPC resolves identity from certificate CN | ✅ done (see `tests/mtls-test.sh`) |
| I2 | **Approval flow**: join pending → owner approve → active; revocation checks, single-use, automatic role grant | ✅ done (revocation check + active disconnect, see `tests/ws-test.ts`) |
| I3 | Plugin integration: mTLS transport + auto-execute wiring | ✅ done (runRpc/runWs mTLS + tools + event-driven) |
| I4 | Cross-network validation (LAN/public internet) | ✅ single-machine LAN simulation passed (`tests/cross-network.sh`); real two-machine setup see `docs/11-test-cross-network.md` |

---

## 10. Risks & Open Questions

| # | Question | Decision |
|---|---|---|
| Q1 | Certificate validity period | **Default 3650 days (10 years)**; plugin prompts renewal before expiry |
| Q2 | CA private-key protection | `~/.teamx/ca/` 0600; optional HSM/keychain support (later) |
| Q3 | Letter transport channel | Offline/encrypted transfer; CLI outputs base64 only and never writes private keys to disk |
| Q4 | Is approve still needed? | **Yes** — certificate means "can connect", approve means "can work"; guards against accidental certificate leakage |
| Q5 | Enforce mTLS? | **Enforced** — network mode is always mTLS, no plaintext downgrade switch |
| R1 | Bun.fetch TLS config support | **✅ verified**: `fetch(url, { tls: { cert, key, ca, serverName } })` fully supports mTLS mutual auth (node https server enforcing requestCert + rejectUnauthorized passed verification; clients without certificates rejected) |
| R2 | rustls dependency is heavy | Accepted; mTLS is a hard requirement |

---

## 11. ADR Summary

1. **Certificate is identity**: establishing the mTLS connection = identity authenticated; member identity/role parsed from the certificate.
2. **Letter is the onboarding package**: certificate+address+role all-in-one; importing = onboarding.
3. **Certificate ≠ authorization**: the certificate allows connecting + submitting requests; owner approve is still required to enter working mode (leak protection).
4. **Enforced mTLS**: network mode has no plaintext downgrade; certificates default to 10 years.
5. **Coexists with V1**: local CLI remains unauthenticated; mTLS applies only to network-mode connections.
3. **Revocation-first**: single-use + revocable; clear security boundary.
4. **Coexists with V1**: local CLI remains unauthenticated; mTLS applies only to network-mode connections.
