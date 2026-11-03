# Vision

## The problem

When you are away from your computer and need a file on it, today's choices are all wrong-sized:

- **Remote desktop** (AnyDesk, TeamViewer, Chrome Remote Desktop, RustDesk): hands over the WHOLE
  machine to see one file. Tiny text, mouse-on-a-touchscreen, and a large attack surface that
  scammers actively exploit.
- **Cloud drives** (OneDrive, Google Drive, Dropbox): the file has to have been put there in advance,
  and you pay with privacy and storage.
- **Sync tools** (Syncthing, Resilio): copy everything everywhere ahead of time. Great for some
  folders, wasteful for "I need that one 2 GB Blender file, once".
- **Self-hosted file servers** (Nextcloud, WebDAV, Tailscale's Taildrive): give you file bytes, but
  nothing helps when the phone cannot open the file (a `.blend`, a CAD file, a big `.psd`).

## The idea

If the phone and the computer already share a private network, use it to give the phone a
**scoped, file-first** view of the computer, and let the user pick, per file, where the work happens:
preview on the computer, fetch to the phone, or open on the computer and stream just that window.

## Who it is for

- Primary: a person with one or more personal computers (desktop, laptop, home server) and a phone
  and/or tablet, who already runs Tailscale or similar.
- Secondary: small households or teams sharing specific folders between their own devices.
- Not for: IT helpdesk / remote support, unattended fleet management, gaming.

## Principles

1. **File-first, not screen-first.** The home screen is your shared folders, not a desktop.
2. **The user chooses where compute happens.** Preview / Fetch / Stream is an explicit choice, with
   a sensible default per file type and per network condition (cellular vs Wi-Fi).
3. **Least access.** Only shared folders, only for paired devices, read-only unless granted.
4. **Bring your own network.** We never run a relay or an account system. If the private network is
   down, the app says so plainly.
5. **Visible to the host.** Anyone sitting at the computer can see it is being accessed and stop it.
6. **Mobile-native.** Thumbnails, offline cache of what you already opened, share-sheet integration,
   touch-first controls in Stream mode. No mouse-cursor-on-a-phone as the primary interaction.
7. **Honest when it cannot.** "This file type cannot be streamed", "host is asleep", "you are on
   cellular, this is 1.8 GB" -- said up front, not discovered after a spinner.

## Non-goals (v1)

- Full desktop control, multi-monitor, gaming-grade latency.
- Two-way sync, conflict resolution, versioning.
- Any cloud component, account system, or NAT traversal of our own.
- Remote support for other people's machines.

## Success looks like

- From cold, the phone shows the shared folder list in under 2 s on a good connection.
- A 50-page `.pptx`: preview first page in under 3 s; stream PowerPoint showing it in under 8 s.
- Nobody can reach a file outside a share, and the host owner always knows when access happens.
