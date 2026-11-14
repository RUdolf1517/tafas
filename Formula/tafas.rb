# Homebrew formula for tafas. Third-party tap: the repository is not named
# `homebrew-tap`, so the tap command names the URL explicitly:
#
#   brew tap RUdolf1517/tafas https://github.com/RUdolf1517/tafas
#   brew install RUdolf1517/tafas/tafas
#
# The four `sha256` values are the hashes of this version's archives, copied from
# the release's own `SHA256SUMS`. They cannot be written before the release
# exists, so filling them in is the one hand edit in the release process
# (docs/releases.md §4 step 5), and it is a follow-up commit on purpose: the
# archive has to exist before its hash does. A formula that shipped zeroes would
# look like a hash and fail at install time instead.
class Tafas < Formula
  desc "Capture a video from YouTube, TikTok, X, Vimeo, Reddit, SoundCloud or Twitch as one playable MP4"
  homepage "https://github.com/RUdolf1517/tafas"
  version "0.3.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-aarch64-apple-darwin.tar.gz"
      sha256 "e4f79006e7b9220617fdfd21cab8a702cfa2116356d53baecb26a13fbe18aa9c"
    end
    on_intel do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-x86_64-apple-darwin.tar.gz"
      sha256 "8e02a05bf9410f48309034017b611fc9fd418061305dc517a62a28925dab9f3f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-aarch64-unknown-linux-musl.tar.gz"
      sha256 "8e58e32226fab58e056a41c1e15af6257b52abacc0d62678e104372cce0f3fba"
    end
    on_intel do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d8781dd5e32e71060368657c34f95f0fb084f53be2d63c7b54a79b715ed4a18b"
    end
  end

  def install
    bin.install "tafas"
    man1.install "man/tafas.1"
    # Completions are generated from the live parser, so they cannot drift from
    # `--help` the way a checked-in script can.
    generate_completions_from_executable(bin/"tafas", "--completions")
  end

  def caveats
    <<~EOS
      tafas writes no configuration file and keeps no state between runs.
      Nothing else needs to be installed: it does not call ffmpeg, yt-dlp or any
      other program.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/tafas --version")
    assert_match "\"sites\"", shell_output("#{bin}/tafas --capabilities")
    # A usage error, made without a network request: exit 2, nothing fetched.
    assert_match "usage:", shell_output("#{bin}/tafas --quality nope https://youtu.be/x 2>&1", 2)
  end
end
