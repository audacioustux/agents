---
name: handling-secrets-and-credentials
description: Use when storing a password, issuing a session token, encrypting data at rest, or choosing a cryptographic primitive — and when reviewing code that does any of those. Covers password hashing parameters, session token entropy and cookie flags, AEAD nonce discipline, and the password-policy rules that most checklists still get backwards. Not for threat modelling, not for infrastructure hardening, not for a specific vault product.
---

# Handling Secrets and Credentials

The rules here are the ones where a plausible-looking choice is wrong, and where
the failure is silent. A too-fast hash, a reused nonce, and a session token
seeded from a timestamp all work in every test you will write.

Every figure is cited. Where a source is only a convention, this says so.

## Role

This skill owns credential storage, token issuance, and primitive selection.
Adjacent concerns that belong elsewhere: what a system should be allowed to do
after authentication is authorization, threat modelling is its own discipline,
and secret *distribution* — vaults, key management services, rotation
infrastructure — is a deployment concern owned by whatever runs the deployment.

## When to use

- Writing or reviewing a signup, login, password reset, or session flow.
- Choosing a hash, cipher, or signature algorithm, or reviewing one already
  chosen.
- Encrypting data at rest, or anything that generates a nonce or IV.
- Reviewing a diff that touches a credential, a token, or a key.

Not for: choosing a vault product, hardening a host, writing a security policy,
or running a scanner. Those are tools and processes, not rules.

## Password policy: the defaults are backwards

Composition rules and forced rotation both make passwords weaker. This is not a
preference — NIST SP 800-63B states it, and the reasoning is that both push
users toward predictable transformations of a short base.

- **No composition rules.** Do not require a mix of character classes. NIST:
  analyses of breached password databases "reveal that the benefit of such rules
  is less significant than initially thought", while "the impacts on usability
  and memorability are severe."
- **No periodic expiry.** Rotate on evidence of compromise, not on a calendar.
  Forced 90-day rotation produces `Summer2024!` then `Autumn2024!`.
- **Length over complexity.** A longer minimum buys more than any class
  requirement. Allow all printable characters including spaces, and do not cap
  length below what the hash accepts.
- **Check against breach corpora.** A password absent from every breach list is
  worth more than one satisfying every composition rule.

The checkable form: a signup form that rejects `correct horse battery staple`
for lacking a digit is violating this, and so is a reset flow that fires on a
timer.

## Password storage

Use a memory-hard function with parameters, not a fast hash. MD5, SHA-1 and
bare SHA-256 are all wrong here for the same reason — they are fast by design,
which is the property an attacker wants.

OWASP's Password Storage Cheat Sheet, current minimums:

| Function | Minimum configuration |
| --- | --- |
| Argon2id | 19 MiB memory, iteration count 2, parallelism 1 |
| bcrypt | work factor 10 or more — **and a 72-byte password limit** |
| PBKDF2-HMAC-SHA256 | 600,000 iterations |
| PBKDF2-HMAC-SHA512 | 220,000 iterations |

Two traps in that table:

**bcrypt silently truncates at 72 bytes.** A longer password is not rejected —
the tail is ignored. Pre-hashing to fit changes the problem rather than solving
it, because a base64-encoded SHA-256 digest is 44 bytes of the 72 and null
bytes in the digest truncate some implementations earlier still. If passwords
may exceed 72 bytes, that is a reason to choose Argon2id.

**PBKDF2 iteration counts age faster than anything else here.** A number
carried from an older document is the most common form of this defect: 100,000
iterations was defensible once and is six times under the current figure.
PBKDF2-HMAC-SHA1 at 1,400,000 is legacy-only and should not be selected for
anything new.

Salt per password, never shared, never derived from the username. Both bcrypt
and Argon2id handle this for you; hand-rolling it is where it goes wrong.

## Session tokens

- **Entropy, not length.** OWASP's floor is 64 bits of entropy, with session
  IDs of at least 128 bits. Take the bytes from a CSPRNG —
  `crypto.randomBytes`, `secrets.token_urlsafe`, `crypto/rand` — never from a
  timestamp, a counter, a UUIDv1, or a PRNG seeded at startup.
- **The predictable-token failure is invisible in testing.** Sequential tokens
  pass every functional test, because functional tests do not guess.
- **Cookie flags are the other half.** `HttpOnly` keeps the token out of reach
  of injected script, `Secure` keeps it off plaintext transport, and `SameSite`
  is what stands between the session and cross-site request forgery. A token
  with perfect entropy in a cookie missing `HttpOnly` is readable by any XSS on
  the page.
- **Rotate the identifier on privilege change.** Issue a new session ID at login
  and at any elevation. Carrying one across the boundary is session fixation:
  an attacker who planted a known ID before login still holds a valid one
  after.
- **Expiry belongs to the server.** `Max-Age` tells a cooperating browser when
  to forget; only server-side invalidation makes a token stop working.

## Encryption

Pick an AEAD — a mode that authenticates as well as encrypts. AES-GCM and
ChaCha20-Poly1305 both qualify. ChaCha20-Poly1305 is the better default where
AES hardware acceleration may be absent.

**The nonce rule is absolute.** Under one key, a GCM nonce must never repeat.
NIST SP 800-38D makes uniqueness a requirement of the mode rather than a
recommendation, and the consequence of breaking it is not degraded security but
collapse: two messages under the same key and nonce leak their XOR, and the
authentication key itself becomes recoverable. Generate nonces randomly at 96
bits, or from a counter that cannot reset — and note that a counter which
restarts when a process restarts is exactly the failure this rule names.

Unauthenticated modes — ECB always, CBC without a separate MAC — do not detect
tampering. ECB additionally preserves plaintext structure: identical blocks
encrypt identically, which is why the encrypted penguin image is still
recognisably a penguin.

Separate keys by purpose. An encryption key is not a signing key is not an API
key, and reusing one across purposes means a compromise in the weakest context
takes the strongest with it.

## Review checklist

- Does any password rule reject a passphrase for its character classes?
- Does anything expire passwords on a schedule rather than on evidence?
- Is the password hash memory-hard, and are its parameters at or above the
  current OWASP minimum rather than a figure inherited from older code?
- If bcrypt: is the 72-byte limit handled or documented?
- Does every session token come from a CSPRNG, and does it carry `HttpOnly`,
  `Secure`, and a `SameSite` value?
- Does the session identifier change at login and at privilege elevation?
- Can any nonce or IV repeat under one key — including after a restart?
- Is the cipher mode authenticated?
- Does any key serve more than one purpose?

## Anti-patterns

| Symptom | What it means |
| --- | --- |
| Password policy demands a symbol and a digit | Composition rules; weakens passwords per NIST |
| `PBKDF2` with 100,000 iterations | A figure from an older document; current minimum is 600,000 for SHA-256 |
| bcrypt with no note about long passwords | Silent truncation at 72 bytes |
| Session ID built from a timestamp or counter | Predictable; passes every functional test |
| Nonce counter initialised at process start | Repeats on restart, which is the catastrophic case |
| `AES-CBC` with no MAC in sight | Unauthenticated; tampering undetectable |
| One key used to encrypt and to sign | A compromise in one context takes both |

## Sources

- NIST SP 800-63B, *Digital Identity Guidelines* — password composition and
  rotation guidance.
- OWASP *Password Storage Cheat Sheet* — Argon2id, bcrypt, PBKDF2 minimums and
  the bcrypt 72-byte limit.
- OWASP *Session Management Cheat Sheet* — session entropy and identifier
  length.
- NIST SP 800-38D — GCM specification, including the nonce-uniqueness
  requirement.

Every parameter above ages. When this skill and a current cheat sheet disagree,
the cheat sheet is right and this file is the defect.
