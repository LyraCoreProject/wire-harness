# Security policy

## What this tool is

`vanilla-wire` is a **client**. It opens outbound connections, speaks SRP6 and the encrypted
build-5875 world protocol, and asserts on what comes back. It listens on no port, runs no server,
and stores no state beyond the files a scenario is explicitly told to write.

The security surface worth reporting is therefore narrow, and mostly this:

* **Credential handling.** Passwords enter only through stdin (`--password-stdin`) and are never
  written to a log, an argv, an environment variable the client sets, or a crash dump. A path that
  leaks one is a real bug — report it.
* **Parsing untrusted server output.** The client decodes frames from whatever it connected to. A
  malicious or broken server should produce an error, not a panic that discards a diagnosis, and
  never a read past a buffer. Decompression is size-bounded on purpose.
* **The crash-dump ring.** On a decode failure the client dumps recent frames for diagnosis. Those
  bytes are session traffic; treat a dump from a production server as sensitive.

Fixture passwords under `adapters/` (`test123`, `seamtest123`) are **not** secrets. They are
throwaway credentials for local dev accounts on a local server, documented as such, and overridable
per account. Using them on anything reachable from a network is the misuse, not the disclosure.

## Reporting a vulnerability

Open a [private security advisory](https://github.com/LyraCoreProject/wire-harness/security/advisories/new)
on this repository. Please do not open a public issue for anything in the categories above until
there is a fix.

Include: what you did, what happened, and what you expected. A packet capture or the raw bytes is
worth more than a description.

Expect an acknowledgement within a week. This is a hobby project maintained in spare time; there is
no SLA and no bounty.

## Supported versions

The most recent tag, and `main`. Nothing older is patched.
