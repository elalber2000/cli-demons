# cli-demons

Run the TUI with:

```sh
cargo run -- spawn
```

Or install the `demon` binary:

```sh
cargo install --path .
demon spawn
```

The left side contains the ASCII art. The right side accepts shell commands.
Type `demon spawn` to generate a random demon, or `demon spawn Zavor` to
summon a named demon. Type `demon kill` and press Enter to exit.
