# The Homebrew Cask for `seam-explorer-egui`, the native macOS build of
# Seam Explorer (https://github.com/esumerfd/arch-visual). This repo is NOT
# named `homebrew-arch-visual`, so Homebrew's one-argument tap shorthand
# does not resolve it -- tap it with the explicit two-argument form:
#
#   brew tap esumerfd/arch-visual https://github.com/esumerfd/arch-visual
#   brew install --cask seam-explorer-egui
#
cask "seam-explorer-egui" do
  arch arm: "aarch64-apple-darwin", intel: "x86_64-apple-darwin"

  version "0.3.0"
  # Real digests, verified against the seam-explorer-egui-v0.3.0 release
  # assets themselves (not just copied from the .zip.sha256 sidecars) --
  # the bundle is not byte-reproducible (cargo-bundle stamps
  # CFBundleVersion with a build timestamp), so these must come from an
  # actual published asset, never computed locally. Kept in sync
  # automatically going forward by this repo's own release.yml (its
  # update-egui-cask job, downstream of esumerfd/actions' checksums.yml).
  sha256 arm:   "4bda07defab32177db52c1d054ea07acdb4ca13c8eb51d6f31c2c3c161fdfd47",
         intel: "79733ce261c371d8ed2fe375e8eeb820f60bd769ae5d0c76c789da61de0d56b3"

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
