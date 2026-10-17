# Accounts, roles and visibility

Context: anyone touching authentication, authorization, sign-in, repository creation, the forge's
permissions, or anything that decides who may read or write a repository. Decision of record: **D51**
(`AGENTS.md` §4). Code: `crates/walgit-wal/src/{accounts,access}.rs`, `crates/walgit-server/src/access.rs`,
`crates/walgit-server/src/web/{login,people}.rs`.

## Model

GitHub's shape, on the bucket.

- **Accounts.** An account is a **user** or a **minimal org**. Its `login` is the `<owner>` namespace.
  - A user has an immutable `user_id` (uuid v4). Login, email, name and avatar are attributes. Roles,
    org ownership and forge authorship are stored against the `user_id`.
  - Sign-in identities (`(issuer, subject)` of an OIDC ID token) link to a `user_id`. They are a list, so a
    second provider can be linked later.
  - An org is a login and its owners (`user_id`s). Members, base permission and teams come later.
- **Password accounts (D52).** With `server.auth.passwords = "admin"` site admins create accounts with a
  password (`walgit user create <login> --password`, the Admin page, `POST /api/v1/users`); with `"open"`
  anyone may also sign up at `/_auth/signup`. People sign in at `/_auth/login` and change their password at
  `/_auth/password`. The hash is Argon2id (a `PasswordSet` account event). The log keeps old hashes. Ten
  failures for one login in 15 minutes lock it out (per instance). Forms are same-origin only.
- **Who creates accounts.** Someone signing in through the identity provider for the first time chooses a
  username at `/_auth/welcome`. Robots and token users are created by an operator: `walgit user create
  <login>`. A static token (`[[server.auth.tokens]] user = "<login>"`) acts as that account. Orgs are created
  by a site admin: `walgit org create <login> --owner <login>` or `POST /api/v1/orgs`.
- **Host flags.** These are not repository permissions.
  - `admin` (a token's `admin`, or oidc `admin_emails` / `admin_domains`) = **site admin**: Admin on every
    repository, may create anywhere and create orgs.
  - `write` (a token's `write`, or oidc `write_domains`) = may **create repositories** under its own login (or
    an org it owns).
- **Roles** on one repository, each including the ones before it:

| Role | May |
|---|---|
| read | clone, fetch, browse (every repository GET), download LFS objects, open issues and pull requests, comment, edit and close your own |
| triage | labels (apply and the catalog), assignees, close and reopen others' items, draft/ready others' pull requests |
| write | push, upload LFS objects, merge, delete a merged branch, edit others' titles, bodies and comments, see collaborators |
| maintain | change settings (`PUT/DELETE …/settings`, `…/settings/validate`), start maintenance ops |
| admin | change visibility and collaborators (`PUT …/access`), `policy.json`, delete the repository |

- **Effective role.** The highest of:
  - site admin → admin;
  - namespace owner (the user, or an owner of the org) → admin;
  - the collaborator role;
  - the visibility floor:
    - **public**: read for everyone (anonymous only when `anonymous_read`);
    - **internal**: read for any account;
    - **private**: nothing.

  One pure function decides it (`walgit_wal::access::role`). The server calls it, and the publisher calls it
  again at every CAS attempt.
- **Participation ≠ read.** Anonymous readers of a public repository may read only. Every forge write needs
  an account. The server answers 401, and the publisher refuses an actor without a `user_id` too.
- **No access** is answered like a repository that doesn't exist: anonymous → 401 (git asks for a credential,
  the UI signs in), signed in → 404. Missing and private are indistinguishable.

## Storage: two WALs

Everything is WAL + bucket (principle I). Both follow the forge (D50): events in log entries, committed by the
manifest CAS, folded in memory, the fold checkpointed as an immutable snapshot, and preconditions re-verified
on every CAS attempt.

1. **The account log** is a WAL with the standard layout at `system/accounts/` (`RepoId::system("accounts")`,
   never routable or listed). `ENTRY_KIND_ACCOUNT` entries carry `AccountEvent`s:
   - `AccountGenesis`
   - `UserCreated`
   - `IdentityLinked`
   - `ProfileChanged`
   - `NamespaceReserved`
   - `OrgCreated`
   - `OrgOwnersChanged`

   The fold (`AccountsState`) derives every index: users, by login, by identity, orgs and reserved namespaces.
   A checkpoint carries `accounts.pb` (`CheckpointRef.accounts_key`); the maintainer writes one when due.

   Preconditions, checked against the CAS basis on every attempt:
   - a login is unique across users, orgs and reserved namespaces;
   - an identity links once;
   - only genesis comes first.
2. **Repository access** is inline on each repository's `manifest.pb` (`Manifest.access` = `RepoAccess`:
   visibility, `user_id → role`, revision), with `ENTRY_KIND_ACCESS` entries as history. It works exactly like
   settings (D24): every refs-level sync carries it for free. A repository's creation CAS includes its initial
   access (`?visibility=`). Absent = public, no collaborators.

## Namespace reservation (security 1)

A namespace that holds repositories but no account is **reserved**. Sign-up can never take it; only a site
admin's org can.
- The account log's genesis entry reserves every owner under `repos/` at that moment (one LIST, once in the
  bucket's lifetime).
- Every later creation in an unowned namespace (by a site admin, `mode = none`, `walgit repo create`,
  `walgit import`) first publishes `NamespaceReserved`, then CAS-creates the repository. A crash in between
  leaves only a harmless reservation.
- Older binaries don't reserve: stop them before the first D51 writer runs (the D43 rule).

## Authentication

`Authenticator::credential` says what a request proved:
- anonymous;
- the local user (`mode = none`);
- a static token (`user` + flags);
- an account (session cookie, `wgt_` token, or a forwarded `X-Walgit-Principal` `user_id` from a trusted
  front's token);
- a verified ID-token identity.

`access::authenticate` resolves it to a `Subject`:
- A token whose `user` has no account, or an identity with no account, is `NoAccount`: 403 with what to do,
  in-band for git.
- An account's flags are those of the static tokens bound to its login, plus (oidc) its email's policy,
  re-applied on every request. An email that left the allowlist is `Forbidden`.
- Session cookies and `wgt_` tokens carry the `user_id`. Rotating `session_secret` still revokes them all.

The fold is read without revalidation for facts that never change once written: an id, an identity link, a
login (until renames exist). A miss syncs the account log once. A profile email change reaches other
instances at their next account-log sync.

## `mode = none` (security 3)

`none` makes everyone a site admin with no credential: one person, one machine.
- `Config::validate` refuses all of these under `mode = none`:
  - a non-loopback `listen` or `public_url`;
  - `trusted_forwarders`, `accel_redirect` or `cors_origins`;
  - `wal.push_broker_url` or `wal.push_broker_token`.
- Per request, `access::local_only` (outermost) answers 403 unless all of these hold:
  - the TCP peer is loopback (`ConnectInfo`, on the TLS listener too);
  - `Host` is `localhost`, `*.localhost`, `127.0.0.1` or `[::1]` (no DNS rebinding);
  - there is no `Forwarded`, `X-Forwarded-*`, `X-Real-IP` or `X-Walgit-*` request header (no local proxy, no
    other walgit host).
- The local user is the sentinel `user_id = "local"` (`login = anon`); it is never in the account log.
  `/api/v1/me` says `auth_mode: "none"`.

## Revocation semantics (security 4)

Access changes are WAL entries, so the manifest CAS orders them against every write.
- **Writes are judged at their CAS basis.** Pushes (`publish_push_as`), forge events and merges carry their
  actor; the publisher re-derives the collaborator role and visibility from the manifest of each attempt.
  - A revocation that commits before a write's CAS refuses it: every ref `ng` "permission changed during
    push", forge events 400, nothing published.
  - A revocation that commits after it leaves it standing.
- **Request-start relations.** Site admin (host config) and namespace ownership (the account log) are resolved
  when the request starts and passed in (`owner_admin`). A change of either reaches a write in flight within
  that request's duration. This is the documented bound.
- **Deleting a merged pull request's branch** is authorized at request time (a ref update without an actor).
- **Reads in flight** (a clone streaming, an SSE task stream) finish with what was authorized when they
  started. The next request is refused (principle IV: `access::authorize` revalidates on every request).
- **A forwarded push** carries the end user's `user_id`; the broker authorizes and judges it.

Tests:
- `walgit-wal`: `test_access_revoked_before_the_cas_refuses_the_write`, the stale-basis case;
  `forge::tests::participation_needs_an_account_and_a_role`.
- `walgit-server/tests/access.rs`: `revocation_applies_to_the_next_request_everywhere`, cross-instance.

## Round trips

See `docs/ROUNDTRIPS.md` (D51 rows):
- `access::authorize` makes the request's refs sync, and the handler's own sync reuses it (`request_scope`).
  Handlers that already synced pay nothing.
- API immutable-cache hits, `ops` / `tasks` lists and task streams gain one conditional GET.
- Org-owner checks sync the account log only when floor and collaborator role don't already grant the need.
- Listings revalidate each listed repository (`ponytail:` a listing index replaces this when owner pages get
  large).

## Surface

| Route | Who |
|---|---|
| `GET /api/v1/me` | any caller: `{login, user_id, name, email, avatar_url, can_create, site_admin, anonymous, auth_mode}` |
| `GET /api[-browser]/v1/users?q=&n=` | any account: login/name prefix search (pickers) |
| `GET /api/v1/orgs/{login}` · `POST /api/v1/orgs` · `PUT /api/v1/orgs/{login}/owners` | any account · site admin · site admin or an owner |
| `GET /{o}/{r}/api` | adds `visibility` and the caller's `role`; the `ETag` covers both |
| `GET\|PUT /{o}/{r}/api[-browser]/access` | write reads; admin replaces `{visibility, collaborators: [{login, role}], revision?}` |
| `PUT /{o}/{r}?visibility=` | create (`can_create` under your login or org; site admin anywhere) |
| `GET\|POST /_auth/welcome` | a pending sign-up chooses a username |
| `GET\|POST /_auth/login` · `GET\|POST /_auth/signup` · `GET\|POST /_auth/password` | password sign-in · sign-up (`open`) · change password (D52) |
| `POST /api/v1/users` · `PUT /api/v1/users/{login}/password` | site admin: create an account (optionally with a password) · reset a password |

CLI (operator, bucket credentials): `walgit user create [--password]|password|ls`, `walgit org create|owners`.
The UI: the top bar's `/sign in` (and `/sign up`) or your `@login` menu; `/new` creates a repository;
`/admin` (site admins) creates accounts and orgs.

## Deferred

- Orgs: members, base permission, teams.
- Login rename with redirects.
- `@mention` autocomplete.
- An org-claim button in the UI.
- Account suspension.
- A repository listing index.
- Re-verifying org ownership and site admin at CAS.
