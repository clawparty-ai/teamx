# teamx.rb — Homebrew formula for the teamx CLI.
#
#   brew tap clawparty-ai/teamx
#   brew install teamx
#
# Downloads the prebuilt CLI binary from the GitHub Release (built by
# .github/workflows/release.yml on each `v*` tag). To enable the opencode
# plugin, run `teamx plugin install` after installing (it wires dist + agent +
# commands into ~/.config/opencode).
class Teamx < Formula
  desc "Shared-goal team collaboration for opencode (AI-native organizations)"
  homepage "https://github.com/clawparty-ai/teamx"
  url "https://github.com/clawparty-ai/teamx/releases/download/v0.3.0/teamx-aarch64-apple-darwin.tar.gz",
      using: CurlDownloadStrategy
  sha256 "e19ac7a274c8b10d8e5c9e13a2ebcb43d29285015fc9853eacaeb91a7385f502"

  on_intel do
    url "https://github.com/clawparty-ai/teamx/releases/download/v0.3.0/teamx-x86_64-apple-darwin.tar.gz",
        using: CurlDownloadStrategy
    sha256 "eea2fc979fb0caaf62cb9bad2c9ca6ce44a35cf02eeff3da71e1fabcb737a40e"
  end

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
