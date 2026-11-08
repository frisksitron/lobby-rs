# Security policy

## Report a vulnerability privately

Do not open a public issue for a security problem. Report it through GitHub's private
vulnerability reporting:

1. On the repository page, open the **Security** tab.
2. Click **Report a vulnerability**.
3. Describe the problem, the Lobby version, and the steps that reproduce it.

Only you and the maintainers can see the report. You get an answer in the same thread. A
confirmed problem is fixed in a new release, and the advisory credits you unless you ask it not
to.

## Supported versions

Only the latest release gets security fixes. When a release changes the protocol, the server and
every app must update together. The app updates itself, and a server operator pulls the new
image.

## What is in scope

Lobby puts its security work into the front door: everything that decides who gets into a Lobby.
Report a problem in any of these:

- Sign in with a Magic Code.
- Magic Codes: how they are made, mailed, expired and limited.
- Admission: Ask to Join and Vouch.
- Sessions and tokens, including how the app stores them.
- Rate limits.
- TLS, for the app's connections and for the server's mail over STARTTLS.
- The app's self-update: the download and its checksum check.
- A Member doing what only the owner may do.
- Anyone who is not a Member reading or changing anything in a Lobby.

## What is out of scope

These are design choices, not vulnerabilities:

- Every Member sees everything in the Lobby. A Member who reads or saves what other Members
  shared is using Lobby as designed.
- Lobby has no encryption at rest. The server stores the database and uploaded files as plain
  files, and whoever runs the server can read them.
- Lobby has no end-to-end encryption. Voice and Screen Share pass through the server, which can
  decode them.
- An attack that needs control of the server or of a Member's computer.
- A problem that exists only in a modified build.
