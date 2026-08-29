# The Homebrew Cask for `seam-explorer-egui`, the native macOS build of
# Seam Explorer (https://github.com/esumerfd/arch-visual). This repo is NOT
# named `homebrew-arch-visual`, so Homebrew's one-argument tap shorthand
# does not resolve it -- tap it with the explicit two-argument form:
#
#   brew tap esumerfd/arch-visual https://github.com/esumerfd/arch-visual
#   brew install --cask seam-explorer-egui
#
# PLACEHOLDER CHECKSUMS -- this cask is not installable until these are
# finalized. Finalization checklist, in order:
#   1. Confirm `version` below against the tag actually cut. This file
#      currently mirrors seam-explorer-egui/Cargo.toml's version at the
#      time this cask was authored; the orchestrator may choose a
#      different first version.
#   2. Download the `.zip.sha256` sidecar asset for EACH architecture from
#      the seam-explorer-egui-v<version> GitHub Release (published by
#      .github/workflows/seam-explorer-egui-release.yml). These are real
#      digests CI already computed -- copy them, do not compute your own.
#      The bundle is not byte-reproducible (cargo-bundle stamps
#      CFBundleVersion with a build timestamp), so no local build can ever
#      reproduce CI's digest.
#   3. Paste each digest into the matching `sha256 arm:`/`intel:` slot
#      below, replacing the placeholder on that line only.
#   4. Re-run `brew style Casks/seam-explorer-egui.rb`, and, from a real
#      tap, `brew audit --cask seam-explorer-egui`.
#   5. Verify with a real `brew install --cask seam-explorer-egui`.
cask "seam-explorer-egui" do
  arch arm: "aarch64-apple-darwin", intel: "x86_64-apple-darwin"

  version "0.1.0"
  # PLACEHOLDERS -- see the finalization checklist above. Deliberately
  # different from each other (Homebrew's Cask/OnSystemConditionals cop
  # rejects identical per-arch checksums) and obviously fake -- no real
  # digest repeats a single hex digit 64 times.
  sha256 arm:   "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
         intel: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

  url "https://github.com/esumerfd/arch-visual/releases/download/seam-explorer-egui-v#{version}/seam-explorer-egui-v#{version}-#{arch}.zip"
  name "Seam Explorer (egui)"
  desc "Visualize architectural seams in a codebase graph"
  homepage "https://github.com/esumerfd/arch-visual"

  livecheck do
    url :url
    strategy :github_releases
    regex(/^seam-explorer-egui-v(\d+\.\d+\.\d+)$/i)
  end

  depends_on macos: :big_sur

  app "Seam Explorer (egui).app"

  postflight do
    # Homebrew 6.x has no `--no-quarantine` install flag, so a cask-installed
    # unsigned app is quarantined unconditionally and macOS refuses to open
    # it. This does exactly what `make install-egui` already does for the
    # local install path -- disclosed in `caveats` below.
    system_command "/usr/bin/xattr",
                   args: ["-cr", "#{appdir}/Seam Explorer (egui).app"]
  end

  zap trash: [
    "~/.config/seam-explorer/settings.json",
    "~/Library/Application Support/Seam-Explorer",
    "~/Library/Saved Application State/com.archvisual.seamexploreregui.savedState",
  ]

  caveats <<~EOS
    Seam Explorer (egui) is unsigned and not notarized -- it is an internal
    tool, not a public release with an Apple Developer certificate.

    This Cask cleared the quarantine attribute during install (the same
    thing `make install-egui` does for a local build), so macOS did not
    prompt you with a Gatekeeper warning. If you ever copy the .app another
    way and see "app is damaged and can't be opened," run:

      xattr -cr "/Applications/Seam Explorer (egui).app"
  EOS
end
