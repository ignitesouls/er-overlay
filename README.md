# er-overlay

This is an in-game overlay for ELDEN RING with the ability to track the following data:

1. Bosses Killed
2. Total Bosses being tracked
3. Number of player deaths
4. Number of Messmer's Kindling Shards in player's inventory
  - This is compatible with Matt's Randomizer (it has custom logic that splits Kindling into shards required burn the sealing tree)
5. Number of Great Runes player has acquired

## How to Use

Visit the [releases](https://github.com/ignitesouls/er-overlay/releases/latest) page. Download and extract the latest release. Include into your modded ELDEN RING game by using modengine3 (recommended) or by using "Add dll mod" in Matt's Item and Enemy Randomizer.

## Changing the overlay display text
Edit the `ignite_overlay_config.toml` file, navigate to this section:
```toml
[overlay]
# how to display the text on the overlay
# display text = "IGT: {igt}$nBosses: {kills}/{total}$nGreat Runes: {runes}$nShards: {shards}$nDeaths: {deaths}"
#  $n = newline
#  {kills} = current kill count
#  {total} = total boss count
#  {deaths}= Death count in current game
#  {igt}   = In-game time
#  {shards}= Number of messmer's kindling shards acquired
#  {runes} = Number of great runes acquired
display_text = "IGT: {igt}$nBosses: {kills}/{total}$nGreat Runes: {runes}$nShards: {shards}$nDeaths: {deaths}"
```
This translates into the overlay displaying:

![alt text](image.png)

## Reporting kills to a tracker (optional)

The overlay can report boss kills to an HTTP endpoint, so a tracker website can
mark a boss the moment it dies instead of you alt-tabbing to click it.

**This is off by default.** Until you paste a token the overlay makes no network
requests at all and behaves exactly as it always has.

To turn it on, edit the `[ingest]` section of `ignite_overlay_config.toml` and
paste the token your tracker gave you:

```toml
[ingest]
url = "https://zltjdeikpsbohvgtmmsn.supabase.co/functions/v1/auto-fire"
token = "your-token-here"
interval_ms = 1000
heartbeat_s = 60
```

The token is permanent — paste it once and it keeps working across matches,
teams and rooms. Clearing it, or deleting the section, turns reporting back off.

What gets sent is only your token and the ids of the boss flags currently
observed as killed. The overlay holds no knowledge of what the tracker does with
them; the server works out the rest.

A few properties worth knowing:

- **Read-only.** The overlay reads event flags, exactly as it already does to
  draw the boss list. It never writes them. This reports kills; it cannot cause
  them.
- **Self-healing.** Every report carries the full kill set, so a dropped request
  or a restart mid-match recovers on the next report. Nothing is lost and
  nothing is double-counted.
- **Fails closed.** If the network is down or the endpoint is unreachable, no
  square gets marked automatically and you click it yourself, which is the
  status quo. It will not affect the game.
- Reloading a save or quitting to the menu is never treated as un-killing a
  boss.

When reporting is on and a match is live, one extra line appears in the HUD,
straight from the server's own numbers:

```
Hit 8   Miss 4   Total 12   Acc 67%
```

A `⚠` on that line means the last report did not land — without it, a stale
tally would look identical to a live one. Set `show_ingest_tally = false` under
`[overlay]` to hide the line.

## Contributing

Want to help shape the overlay? Join the [Ignite discord](https://discord.gg/ignitesouls) and contact a @firekeeper to learn more about our mod team.

---

### Credits
- [Sully-](https://github.com/Sully-): for showing me hudhook, sharing code examples, and helping me with debugging the overlay
- [hudhook](https://github.com/veeenu/hudhook): a rust crate for creating in-game UI overlays, made by Andrea
- [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs): a collection of rust crates for interacting with elden ring specifically, made by Vswarte

## Licensing

SPDX-License-Identifier: GPL-3.0-only