# Security

LagScope runs with administrator rights (Windows requires them for its event tracing), so
security problems matter more than usual: anything that lets another program or a file
make LagScope do something it should not (load a DLL, run a command, write outside its
folders, change settings without asking), or that leaks private data into recordings.

## Reporting

Please do not open a public issue. Use GitHub's private reporting instead: the
**Security** tab of the repository > **Report a vulnerability**. You will get an answer
within a week. Once a fix is released, the report is published with credit to you, unless
you prefer otherwise.

## Supported versions

Only the latest release gets fixes. LagScope has no auto-update; the release notes say
when an update fixes a security problem.
