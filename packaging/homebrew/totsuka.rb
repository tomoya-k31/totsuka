# Homebrew formula template for totsuka.
#
# This lives in the tap repository `tomoya-k31/homebrew-totsuka` as
# `Formula/totsuka.rb`. On each release the `universal-binary` job in
# `.github/workflows/release-please.yml` produces the tarball and its raw
# `.sha256`; bump `version`, `url`, and `sha256` here per the manual runbook,
# docs/operations/release-runbook.md. `VERSION`/`SHA256` are placeholders
# replaced at bump time.
class Totsuka < Formula
  desc "AI-driven dev-flow automation: detect task instructions and orchestrate them to AI agents"
  homepage "https://github.com/tomoya-k31/totsuka"
  version "VERSION"
  url "https://github.com/tomoya-k31/totsuka/releases/download/vVERSION/totsuka-vVERSION-macos-universal.tar.gz"
  sha256 "SHA256"
  license "MIT"

  depends_on "git"

  def install
    bin.install "totsuka"
  end

  test do
    assert_match "totsuka", shell_output("#{bin}/totsuka --version")
  end
end
