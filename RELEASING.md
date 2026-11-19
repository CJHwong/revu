# Releasing

1. Set the version in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and commit them as `chore(release): X.Y.Z` on a branch. Open a pull request and rebase-merge it when its checks pass. main takes changes only through a pull request.
2. Tag the merged commit and push the tag:
   ```sh
   git switch main && git pull
   git tag -a vX.Y.Z -m "revu X.Y.Z"
   git push origin vX.Y.Z
   ```
   The `release` workflow runs the checks, builds the four binaries and publishes them with `checksums.txt` as a GitHub Release. `install.sh` picks up the new release at once.
3. Replace the release body, which GitHub generates as a changelog link only, with the notes: `gh release edit vX.Y.Z --notes-file notes.md`.
4. Update the Homebrew formula by hand. The workflow's `homebrew` job skips it, because the repo has no `HOMEBREW_TAP_TOKEN` secret:
   ```sh
   gh release download vX.Y.Z -R CJHwong/revu -p checksums.txt
   git clone https://github.com/CJHwong/homebrew-tap.git
   sh packaging/homebrew-formula.sh X.Y.Z checksums.txt > homebrew-tap/Formula/revu.rb
   cd homebrew-tap
   git commit -am "revu X.Y.Z" && git push
   ```
5. Check it: `brew update && brew info CJHwong/tap/revu` shows the new version, and `brew fetch CJHwong/tap/revu` verifies the checksum.
