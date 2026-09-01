# teamx.rb — Homebrew formula for the teamx CLI.
#
#   brew tap clawparty-ai/teamx
#   brew install teamx
#
# Downloads the prebuilt CLI binary from the GitHub Release (built by
# .github/workflows/release.yml on each `v*` tag). To enable the opencode
# plugin, run `teamx plugin install` after installing (it wires dist + agent +
# commands into ~/.config/opencode).
#
# NOTE: the SHA256 below is a placeholder — update it from the release asset
# (see README of this tap, or `shasum -a 256` on the downloaded tarball).
class Teamx < Formula
  desc "Shared-goal team collaboration for opencode (AI-native organizations)"
  homepage "https://github.com/clawparty-ai/teamx"
  url "https://github.com/clawparty-ai/teamx/releases/download/v0.2.0/teamx-aarch64-apple-darwin.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  depends_on "opencode" => :recommended

  def install
    bin.install "teamx"
  end

  def caveats
    <<~EOS
      teamx CLI installed. To use it inside opencode:

        teamx plugin install

      This copies the teamx plugin (dist + agent + /team commands) into
      ~/.config/opencode, then restart opencode. Type /team to get started.

      To uninstall the plugin pieces later: teamx plugin uninstall
    EOS
  end

  test do
    assert_match "teamx", shell_output("#{bin}/teamx --version")
  end
end
