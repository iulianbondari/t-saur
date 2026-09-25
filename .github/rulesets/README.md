# Branch ruleset for `main`

`main.json` is the ruleset the maintainer applies to `main`: no deletion, no force-push, changes
only through pull requests (review threads resolved; no approval count, because the project has
one maintainer), and the CI checks of `.github/workflows/ci.yml` that run on pull requests
required before merging: `test (ubuntu-latest)`, `test (windows-latest)` and `determinism`.
The macOS job runs on every push to `main` and on manual runs (macOS minutes cost ten times the
Linux rate), so it is not a pull-request check. Documentation-only changes do not trigger the
workflow at all; once the ruleset is active such a pull request has no checks to satisfy, and
the maintainer either starts the workflow by hand (`workflow_dispatch`) or, better, gives the
ruleset a second condition that exempts documentation paths.

GitHub offers rulesets and branch protection on private repositories only with a paid plan; on a
public repository they are free. The file is therefore kept here and applied at publication:

```bash
gh api -X POST repos/iulianbondari/t-saur/rulesets --input .github/rulesets/main.json
```

(`gh api -X PUT repos/.../rulesets/<id> --input ...` updates it later.) Until then, `main` is
protected by practice, not by the platform: every change goes through a pull request and CI.
