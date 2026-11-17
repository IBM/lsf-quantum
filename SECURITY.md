# Security Policy

## Reporting a vulnerability

Please do **not** open a public issue for a security problem.

Report it privately by either route:

- **GitHub:** use [Report a vulnerability](https://github.com/IBM/lsf-quantum/security/advisories/new)
  on the repository's **Security** tab.
- **IBM PSIRT:** see <https://www.ibm.com/trust/security-psirt> for IBM's
  vulnerability reporting process.

Include the affected component (`esub-qrmi`, `jobstarter-qrmi`, `elim-qpu`,
`lsfq-core` or `qpu-select`), the version or commit, the steps to reproduce,
and the impact you expect. Never include a real IBM Quantum API key or CRN in
a report.

## Supported versions

Only the latest release and the `main` branch receive security fixes.

## Scope

These plugins run inside an LSF cluster and handle IBM Quantum credentials
(`QRMI_IBM_QCS_IAM_APIKEY` and the service CRN) on behalf of submitting users.
Reports about credential exposure, privilege boundaries between the LSF
daemons and job users, and unsafe handling of `bsub` input are especially
welcome.

Vulnerabilities in dependencies, including [QRMI](https://github.com/qiskit-community/qrmi),
should be reported to those projects. Advisories we have reviewed, and why
some are suppressed in `deny.toml`, are recorded in
[SECURITY-NOTES.md](SECURITY-NOTES.md).
