# kiss-xmpp

`kiss-xmpp` is the simplest XMPP client possible (and usable for IRC through biboumi). It supports a single 1:1 or MUC chat at a time.
It's primary use-case is doing XMPP & IRC over Telnet from a stock Atari 800XL (64kb RAM), which has, let's say, less than ideal vt100 emulation.

Building from git:
  `cargo build --release`

Or grab a binary from the releases section.

Configuration: `cp kiss-xmpp.toml ~/.config/` and edit `~/.config/kiss-xmpp.toml` with your XMPP credentials

```
usage: kiss-xmpp [-c /path/to/config.toml] user@domain OR room@domain/your-room-nick
```

Put jid/pass in `kiss-xmpp.toml`, see example config for format.

License
-------
GNU/AGPLv3 - Check LICENSE.md for details
