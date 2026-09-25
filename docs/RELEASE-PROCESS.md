# Publication and release process

Two procedures: **A** is done once, when the repository goes public; **B** is done for every
version. Both are executed by the maintainer, by hand, with the checks written next to each
step; nothing here is automated beyond the workflows in `.github/workflows/`.

## A. Publication (once)

Precondition: no open pull request (a history rewrite invalidates open branches).

### A1. Clean history

The maintainer's working material (`docs/research/`, `benchmarks/check_names.sh`) was removed
from the tree on 2026-09-25 (`docs/PROVENANCE.md`) but is still present in earlier commits.
Before the repository becomes public the history is rewritten once, **while the repository is
still private** (the only situation in which a forced update of `main` is acceptable here).

Rehearsed on 2026-09-25 in a scratch clone with `git-filter-repo` 2.x:

```bash
git clone --no-single-branch https://github.com/iulianbondari/t-saur rewrite && cd rewrite
BEFORE=$(git rev-parse main^{tree})
git filter-repo --path docs/research --path benchmarks/check_names.sh --invert-paths --force
test "$(git rev-parse main^{tree})" = "$BEFORE" && echo "tree identical"        # must print
test "$(git log --all --oneline -- docs/research benchmarks/check_names.sh | wc -l)" -eq 0
git log --oneline main | wc -l                                                    # 33 in the rehearsal
git rev-list --max-parents=0 main                                                 # the new import commit
```

Rehearsal result: tree at the tip identical, 33 commits at the time, zero commits touching the
removed paths, new root `29bc672` in place of `16eeeec`. **Executed on 2026-09-25** on the
43-commit history (`docs/PROVENANCE.md` has the record), with a mailmap that also normalised
the author and committer identities. Then:

1. `git remote add origin https://github.com/iulianbondari/t-saur && git push --force origin main`
   (the ruleset is not active yet; this is the one forced push, recorded here).
2. Delete the merged branches that GitHub kept, if any (`git push origin --delete <branch>`), so
   that no ref points at the old history.
3. Record in `docs/PROVENANCE.md`: the rewrite, the date, the old and new root commits, and the
   tree hash that proves the tip is unchanged. Commit through a pull request as usual.
4. Let the CI run on `main` once (push event: all three systems) and check that it is green.
5. Refresh the local mirror (`git fetch origin && git reset --hard origin/main` in a clean
   clone; the private development mirror keeps its own history).

### A2. Visibility, rules, preview

1. GitHub → Settings → General → Danger zone → *Change repository visibility* → Public.
2. Apply the ruleset: Settings → Rules → Rulesets → *Import a ruleset* with
   `.github/rulesets/main.json` (or `gh api -X POST repos/iulianbondari/t-saur/rulesets --input .github/rulesets/main.json`).
   Check afterwards that a test pull request shows the three required checks.
3. Settings → General → *Social preview* → upload `docs/brand/tsaur-social-preview.png`
   (1280 × 640). The option appears only on a public repository.
4. Confirm the settings chosen on 2026-09-25 survived the visibility change: release
   immutability on, rebase merging off, "always suggest updating pull request branches" on,
   automatic deletion of head branches on, Dependabot alerts, dependency graph and malware
   alerts on, security and version updates off (updates go through pull requests and the gate).
5. Settings → Advanced Security → *Private vulnerability reporting* → Enable: `SECURITY.md` and the
   issue template send reporters there, and it is off by default on a newly public repository.
6. Repository page → *About* → topics (`archive-format`, `archiver`, `compression`, `rust`,
   `content-addressed`, `deduplication`, `erasure-coding`, `ai-agents`, `mcp`,
   `post-quantum-cryptography`, `blake3`, `zstd`) and, if wanted, a homepage.
7. Wikis stay off (documentation lives in `docs/`); Discussions are the maintainer's call.

### A3. Independent review

Open an issue "Independent review of 1.0.0-rc" that links `docs/review/REVIEW-PACKAGE.md` and
the verification reports, states the scope (the same as the package's §2) and what the reviewer
gets (credit in `CHANGELOG.md` and the report published under `docs/review/`). The review is the
evaluator's own work; the producer's checks are not a substitute.

### A4. Two-device measurements

Run `docs/design/TWO-DEVICE-BENCHMARK-PLAN.md` §0 on two real machines of the maintainer (one
Ethernet run, one Wi-Fi run; `--plain` once for comparison). Commit the generated
`RESULTS-two-devices.md` as `benchmarks/RESULTS-two-devices.md` with the commit hash of the
binaries. A run with both roles on one machine is labelled by the tool as a tooling check and
does not count.

## B. Cutting a release (every version)

1. **Gate.** On the release commit: `python tools/release_gate.py --robustness-seconds 60
   --report docs/review/RC<n>-VERIFICATION-REPORT-<platform>.md` on at least Windows and Linux,
   both release binaries built first; all steps must pass. Any golden test failure is a format
   event: stop, record the first differing bytes, decide.
2. **Version.** Bump `version` in `tsaur/Cargo.toml` (`[workspace.package]`), `version` and
   `date-released` in `CITATION.cff`, the heading in `CHANGELOG.md`; `cargo metadata --locked`
   must still succeed with `Cargo.lock` unchanged except for the workspace crates. Pull request,
   CI green, merge.
3. **Tag.** `git tag -a v<version> -m "T-saur <version>" <merge commit> && git push origin v<version>`.
   The tag starts `.github/workflows/release.yml`: lite packages for the three systems, tests
   run on each, `SHA256SUMS`, and a **draft** release.
4. **Check the draft.** Download each package, verify it against `SHA256SUMS`, unpack in a clean
   directory and run `python tools/check_guide.py --tsaur <unpacked binary>`; compare the sizes
   with `dist/PACKAGES.md` of the local gate run. Fill the release notes from `CHANGELOG.md`;
   state that the full build is available from source only (`docs/DISTRIBUTION-POLICY.md`).
5. **Publish** the draft. Release immutability is on: the assets and the tag cannot change
   afterwards; a mistake means a new patch version.
6. **Registries** (once the maintainer has the accounts): `cargo publish -p tsaur-core` then
   `cargo publish -p tsaur` from the tagged commit; PyPI and npm follow the bindings on the
   roadmap. Never publish from a dirty tree or an untagged commit.
7. **After.** `ROADMAP.md` and `docs/V1-CONTRACT.md` §5 updated with what actually ran; the
   next `## [Unreleased]` section opened in `CHANGELOG.md`.

## C. What must never happen

* A forced push to `main` after publication (A1 is the single exception, before it).
* A release from a commit the gate did not pass, or with a golden test changed "to make it pass".
* A package uploaded by hand without `SHA256SUMS` from the workflow.
* Publishing anything from the maintainer's private mirror instead of this repository.
