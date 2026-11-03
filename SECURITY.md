# Security policy

Gander is a way into someone's computer, so security reports are taken seriously. The threat
model and the non-negotiable design rules are in [`docs/security.md`](./docs/security.md).

_Draft written during the scaffold (WP-01); the maintainers finalise the wording, the contact
address and the response targets before the first public release (WP-83)._

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Send the report privately to:

> `<SECURITY-CONTACT-ADDRESS: placeholder, to be set by the maintainers before the first public release>`

Until that address exists, there is no public release to report against. Once the repository is
public, GitHub private vulnerability reporting ("Report a vulnerability" on the Security tab)
will be enabled as a second channel.

Please include what you found, the affected version or commit, steps to reproduce, and the
impact you expect. Do not include real tokens, pairing secrets or private files.

What to expect (targets for a small project, not contractual promises):

- acknowledgement within 7 days;
- a first assessment within 14 days;
- a fix or mitigation plan before any public disclosure, with coordinated disclosure after a fix
  is available or 90 days have passed, whichever comes first;
- credit in the release notes if you want it.

## Supported versions

Gander has no released versions yet. Until the first release, only the latest commit on the
default branch is supported. Once releases exist, this table will list which ones receive fixes.

| Version | Supported |
| --- | --- |
| unreleased (default branch) | yes |
| anything else | no |

## No bug bounty

Gander is an open-source project with no revenue and **does not run a bug bounty program**.
Reports are welcome and appreciated, and we will say thank you publicly, but we cannot pay for
them.

## Scope

In scope: the Gander host, the Gander Android client, the protocol between them, the installer,
and the build and release tooling in this repository.

Out of scope: vulnerabilities in the private network software Gander runs over (Tailscale,
Headscale, ZeroTier, WireGuard), in the operating system, or in third-party apps that Stream
mode opens; please report those upstream. Findings that require an attacker who already has
administrator rights on the host, or physical access to an unlocked device, are generally out of
scope as well.
