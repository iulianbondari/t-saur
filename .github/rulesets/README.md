# Branch ruleset for `main`

`main.json` is the ruleset the maintainer applies to `main`: no deletion, no force-push, changes
only through pull requests (review threads resolved; no approval count, because the project has
one maintainer), and the four CI checks of `.github/workflows/ci.yml` required before merging.

GitHub offers rulesets and branch protection on private repositories only with a paid plan; on a
public repository they are free. The file is therefore kept here and applied at publication:

```bash
gh api -X POST repos/iulianbondari/t-saur/rulesets --input .github/rulesets/main.json
```

(`gh api -X PUT repos/.../rulesets/<id> --input ...` updates it later.) Until then, `main` is
protected by practice, not by the platform: every change goes through a pull request and CI.
