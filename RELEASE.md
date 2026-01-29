# Release Process

This document describes the release process for the Priority LFU Cache project.

## Overview

The project uses an automated release workflow powered by GitHub Actions and `cargo-release`. There are two release methods:

### Method 1: Standard Release (Recommended)

1. **Prepare Release** - Create a PR with version bumps
2. **Automatic Publishing** - Merge triggers tagging, crates.io publishing, and GitHub release creation

### Method 2: Manual Release

Manually trigger the "Tag and publish release" workflow to release the current state of `main` without creating a version bump PR.

## Prerequisites

Before starting a release, ensure:

- [ ] All changes intended for the release are merged to `main`
- [ ] CI tests are passing on `main`
- [ ] You have appropriate repository permissions
- [ ] `CARGO_REGISTRY_TOKEN` secret is configured in the repository settings

## Standard Release Process

### 1. Initiate Release Preparation

Trigger the "Prepare release" workflow:

1. Go to **Actions** → **Prepare release** in the GitHub repository
2. Click **Run workflow**
3. Enter the version number (e.g., `0.2.0`) in semantic versioning format (`X.Y.Z`)
4. Click **Run workflow**

**What this does:**
- Validates the version format
- Bumps version in all `Cargo.toml` files using `cargo-release`
- Updates `Cargo.lock`
- Runs semver compatibility checks (informational, non-blocking)
- Runs full test suite with all features
- Creates a pull request with the changes

### 2. Review the Release PR

The workflow creates a PR titled `Release vX.Y.Z` with:
- Updated versions in workspace `Cargo.toml` files
- Updated `Cargo.lock`
- Commit message: `chore(release): bump version to X.Y.Z`

**Review checklist:**
- [ ] Version numbers are correct in all crates
- [ ] `Cargo.lock` is updated
- [ ] Tests pass
- [ ] Semver compatibility check results (if any breaking changes are expected)

### 3. Merge the Release PR

Once reviewed and approved, merge the PR to `main`.

**Important:** The PR must be merged with a commit message starting with `chore(release): bump version to` (the workflow sets this automatically).

### 4. Automatic Publishing

After the PR is merged, the "Tag and publish release" workflow automatically triggers:

1. **Extracts version** from the commit message (`chore(release): bump version to X.Y.Z`)
2. **Creates and pushes git tag** `vX.Y.Z`
3. **Publishes to crates.io** (all workspace crates)
4. **Creates GitHub release** with auto-generated release notes

**Monitor the workflow:**
- Go to **Actions** → **Tag and publish release**
- Watch the workflow execution
- Verify successful completion of all steps

### 5. Verify the Release

After the workflow completes, verify:

- [ ] Git tag `vX.Y.Z` exists: `git fetch --tags && git tag -l "vX.Y.Z"`
- [ ] Crates published to crates.io:
  - https://crates.io/crates/priority-lfu
  - https://crates.io/crates/priority-lfu-derive
- [ ] GitHub release created: https://github.com/surrealdb/priority-lfu/releases/tag/vX.Y.Z
- [ ] Release notes generated correctly

## Manual Release Process

Use this method if you need to quickly release without going through the PR workflow, or if the automatic release from a PR merge failed and needs to be re-run.

**⚠️ Warning:** This releases the current state of `main`. Ensure version numbers in `Cargo.toml` files are already updated before triggering.

### Steps

1. **Ensure versions are updated** in all `Cargo.toml` files if not already done
2. Go to **Actions** → **Tag and publish release**
3. Click **Run workflow**
4. Enter the version number (e.g., `0.2.0`)
5. Click **Run workflow**

**What this does:**
- Validates the version format
- Creates and pushes git tag `vX.Y.Z`
- Publishes all workspace crates to crates.io
- Creates GitHub release with auto-generated notes

### When to Use Manual Release

- **Recovery**: The automatic release workflow failed after merging a release PR
- **Hotfix**: Need to quickly release a critical fix without full PR review cycle
- **Re-tag**: Need to recreate a release with the same version (after deleting the old tag)

**Note:** When using manual release, you should still follow the versioning guidelines and ensure all tests pass.

## Versioning Guidelines

This project follows [Semantic Versioning 2.0.0](https://semver.org/):

- **Major version (X.0.0)**: Breaking changes to public API
- **Minor version (0.X.0)**: New features, backwards-compatible
- **Patch version (0.0.X)**: Bug fixes, backwards-compatible

### Pre-1.0 Versioning

While in 0.x versions:
- Breaking changes increment the **minor** version (0.X.0)
- New features and bug fixes increment the **patch** version (0.0.X)

## Alternative: Publish-Only Workflow

If you only need to publish to crates.io (without creating tags or GitHub releases):

1. Go to **Actions** → **Publish release**
2. Click **Run workflow**
3. Enter the version number
4. Click **Run workflow**

**Note:** This workflow only publishes to crates.io and creates a GitHub release. It does not create git tags. This is rarely needed - use the "Tag and publish release" manual workflow instead.

## Troubleshooting

### Release PR Not Created

- Check the workflow logs for errors
- Verify version format is correct (`X.Y.Z`, no `v` prefix)
- Ensure `cargo-release` installation succeeded

### Publishing Failed

- Check `CARGO_REGISTRY_TOKEN` secret is valid
- Verify crate names are available on crates.io
- Check for dependency version conflicts
- Review cargo-release output for specific errors

### Tag Already Exists

If you need to re-release:
1. Delete the tag locally and remotely:
   ```bash
   git tag -d vX.Y.Z
   git push origin :refs/tags/vX.Y.Z
   ```
2. Re-run the appropriate workflow

### Partial Publish

If only some crates published:
1. Check which crates failed in the workflow logs
2. Fix the issues (if any)
3. Re-run the "Tag and publish release" workflow manually with the same version
4. Alternatively, run locally:
   ```bash
   cargo release publish --workspace --execute --no-confirm
   ```

## Rollback

If you need to yank a published version:

```bash
cargo yank --vers X.Y.Z priority-lfu
cargo yank --vers X.Y.Z priority-lfu-derive
```

**Note:** Yanking does not delete the version, it only prevents new projects from depending on it.

## Release Checklist Summary

- [ ] All changes merged to `main`
- [ ] CI passing
- [ ] Run "Prepare release" workflow with version
- [ ] Review and approve release PR
- [ ] Merge release PR
- [ ] Monitor automatic publish workflow
- [ ] Verify git tag created
- [ ] Verify crates.io publish
- [ ] Verify GitHub release created
- [ ] Update documentation if needed
- [ ] Announce release (if applicable)

## Tools Used

- **cargo-release**: Automates version bumping and publishing
- **cargo-semver-checks**: Validates semantic versioning compatibility (informational)
- **GitHub Actions**: Orchestrates the entire release process

## Notes

- The workspace uses Cargo 2024 edition with workspace resolver version 3
- All crates in the workspace are published together
- Version numbers are synchronized across all workspace crates
- Release notes are auto-generated from commit messages between releases
