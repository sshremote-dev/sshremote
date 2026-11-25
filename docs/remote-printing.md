# Remote printing (planned)

What the host prints comes out of a printer of the computer viewing it, as RustDesk 1.4 does
between two Windows computers. Not built yet; this note keeps where it plugs in.

## Flow

1. The host has a printer of SSH Remote's own. A document printed to it becomes a PDF in a spool
   folder of the program.
2. The agent sees the new job and tells the viewer (a new `ToClient` message: the job's name, its
   size and its path in the spool).
3. The viewer fetches it with the existing transfer (`ToAgent::Fetch`, with resume), then prints
   it on the printer chosen there (or asks which), and the host deletes it from the spool.

## Where each part goes

| Part | Place | Notes |
|---|---|---|
| Messages | `src/proto.rs` | Kinds are a byte: about 100 of each side's are free. A viewer or agent older than the feature skips the new kind once with a notice (`proto::is_unknown`). |
| Telling each side can | `ToClient::Can` (`CAN_*`, 31 bits free) and `Settings::formats` (3 bits free) | The viewer offers printing only to an agent that says it can, and the agent sends jobs only to a viewer that says it prints. |
| The job's bytes | `ToAgent::Fetch` / `FetchStart` / `FetchAt` | Resumable already; a job is a file. |
| Who may print | `src/grant.rs` | The rights byte is full (seven rights and the ask flag). Either printing counts as Files, or the rights grow to two bytes: `ToClient::Allowed` can take a second byte after the first (older viewers read one), `remote-access.json`'s `allow` is a number already, and the agent's `--grant` would parse a wider one. |
| The host's printer, Windows | `src/service.rs` install and uninstall | Windows' own "Microsoft Print to PDF" driver on a local port that writes into the spool folder: no driver of our own to sign. The service (SYSTEM) owns the folder. |
| The host's printer, macOS and Linux | the same install step | A CUPS queue whose backend writes the PDF to the spool folder (`lpadmin`, needs an administrator once). |
| Watching the spool | a new `src/print/` module beside `capture/` | One file per system, as `capture/` does. |
| Printing on the viewer | `src/print/` | macOS and Linux: `lp`. Windows: the PDF's pages drawn with `Windows.Data.Pdf` (in the `windows` crate already) onto the printer's page. |
| Turning it off | `src/modules.rs` `ALL` | A part with an on/off switch, as Files or Sound. |
| The window | `src/gui/remote/toolbar.rs`, Settings | The printer to use, and whether to ask each time. |
