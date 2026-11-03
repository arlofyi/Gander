# Naming

> **Decided 2026-11-01: Gander.** See `docs/decisions/0004-name.md`. The rest of this file is the
> research that led there.

## On "OTG View"

Recommendation: **do not ship as OTG View.**

- "OTG" already means **USB On-The-Go** to most Android users (OTG adapters, "OTG settings").
  Searches and app-store listings for "OTG View" will be full of USB camera/endoscope viewer apps.
- "View" undersells it: Fetch and Stream are more than viewing.
- It is fine as a codename while building.

## What the name should carry

- Reaching your OWN machine from afar, through something private.
- The choice of where it runs (here or there).
- Short, pronounceable, works as a verb or a place ("open it in X").
- Must not use a VPN vendor's mark (no "Tail-" anything) -- the app is network-agnostic.

## Candidates

| Name | Why | Risk to check |
| --- | --- | --- |
| **Porthole** | A small window into your machine -- exactly the "one window, not the whole desktop" idea | Existing dev tools named Porthole |
| **Yonder** | "Over yonder" -- your files, over there; friendly | Common word, several small apps |
| **Hither** | Pairs with yonder: bring it hither (Fetch) | Slightly archaic |
| **Farside** | Reach the far side of your network | Existing products / comic |
| **Homeport** | Your home machine as the port you sail from | Marine businesses |
| **Tether** | Phone tethered to computer | Strong collision with phone hotspot "tethering" |
| **Relay / Bridge / Link** | Obvious | Too generic, heavily used |
| **Either** | Open it on either device | Hard to search |

**Shortlist: Porthole, Yonder.** Porthole is the most descriptive of the Stream idea; Yonder is the
warmest and works for the whole app.

## Before choosing

- [ ] GitHub org/repo name free
- [ ] Domain (`.app` / `.dev`) available
- [ ] Google Play + App Store search for direct collisions
- [ ] Trademark search (USPTO TESS, EUIPO, UK IPO) in software classes (9, 42)
