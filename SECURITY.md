# Security

TakeTwo runs on your own computer. Its engine listens only on `127.0.0.1` and refuses requests whose Host or
Origin isn't this computer, so other websites can't drive it. It has no password: any program already
running under your account can use it.

## Reporting a problem
Please report security problems privately through
[GitHub's private advisory form](https://github.com/fnikobanagyo-alt/taketwo/security/advisories/new),
not as a public issue. Include what you found and how to reproduce it. You'll get a reply as soon as possible.

Especially welcome: ways for a web page or another user to reach the engine, read or delete the library, or
run commands through the update or folder-opening features.
