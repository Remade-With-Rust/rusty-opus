# Security Policy

rusty-opus decodes Opus packets that usually arrive from a network peer, so a bug in
the decoder can be reachable by an attacker. We treat security reports as the highest
priority work on the project.

## Supported versions

| Version | Supported |
|---------|-----------|
| 1.x     | Yes — security fixes are released as patch versions |
| < 1.0   | No — please upgrade to the latest 1.x release |

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately through GitHub's advisory form:
<https://github.com/Remade-With-Rust/rusty-opus/security/advisories/new>

Include, where you can:

- the affected version or commit;
- a minimal input that reproduces the problem (an Opus packet or packet sequence, the
  decoder sample rate and channel count, or the encoder configuration);
- the observed behaviour (panic message, sanitizer report, wrong output) and why you
  believe it is a security issue.

## What to expect

| Stage | Target |
|-------|--------|
| Acknowledgement of your report | within 3 business days |
| Initial assessment and severity | within 10 business days |
| Fix or mitigation for critical/high severity | within 30 days of confirmation |
| Fix for medium/low severity | next scheduled release |

We will keep you informed as the fix progresses, credit you in the advisory and
release notes unless you prefer otherwise, and coordinate a disclosure date with you.

## Disclosure policy

We follow coordinated disclosure. Once a fix is available we:

1. publish a patched release to crates.io;
2. publish a GitHub Security Advisory (which is mirrored to the RustSec advisory
   database), describing the impact, affected versions, and the fixed version;
3. note the change under a **Security** heading in [CHANGELOG.md](CHANGELOG.md).

If a fix is not possible within 90 days of the report, we will agree with the reporter
on a disclosure date and publish mitigation guidance.

## Scope

In scope: memory-safety violations, panics, unbounded resource consumption or
infinite loops reachable from packet or PCM input through the public API
(`OpusDecoder`, `OpusEncoder`, `repacketizer`, `multistream`, `parallel`), and
undefined behaviour in any `unsafe` code.

Out of scope: audio-quality differences that are not reachable as a crash or hang,
and issues in the internal modules hidden from the documentation when used outside
the public API's preconditions.

The threat model behind this policy is documented in
[docs/threat-model.md](docs/threat-model.md).
