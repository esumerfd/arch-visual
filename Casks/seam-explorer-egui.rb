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

  version "0.1.0"
  # Real digests, copied verbatim from the .zip.sha256 sidecars CI published
  # alongside the seam-explorer-egui-v0.1.0 release assets -- the bundle is
  # not byte-reproducible (cargo-bundle stamps CFBundleVersion with a build
  # timestamp), so these are read from CI's own output, never computed locally.
  sha256 arm:   "8d19888110d9f14ddccbb64a24a3133694fbda62bc680f957b586977a787c6db",
         intel: "8ab609134825919e675d629e4393a6bc71e600f878650aa4118d7a36df89f043"

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
