# Branch ruleset for `main`

`main.json` is the ruleset the maintainer applies to `main`: no deletion, no force-push, changes
only through pull requests (review threads resolved; no approval count, because the project has
one maintainer), and the CI checks of `.github/workflows/ci.yml` that run on pull requests
required before merging: `test (ubuntu-latest)`, `test (windows-latest)` and `determinism`.
The macOS job runs on every push to `main` and on manual runs (macOS minutes cost ten times the
Linux rate), so it is not a pull-request check. Documentation-only changes do not trigger that
workflow; `.github/workflows/ci-docs.yml` runs instead, on exactly the paths ci.yml ignores, with
jobs of the same names that build nothing and succeed, so that the required checks are reported
and such a pull request can be merged (a path-filtered required check that never runs would block
the merge forever). The two path lists must stay identical. `supply-chain.yml` is deliberately not
a required check for the same reason: it runs only when the dependency graph changes.

GitHub offers rulesets and branch protection on private repositories only with a paid plan; on a
public repository they are free. The file was applied at publication (2026-09-25); to re-apply or
update it:

```bash
gh api -X POST repos/iulianbondari/t-saur/rulesets --input .github/rulesets/main.json
```

(`gh api -X PUT repos/.../rulesets/<id> --input ...` updates it later.)
