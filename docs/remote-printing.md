# Remote printing

What the host prints comes out of a printer of the computer viewing it.

## Flow

1. The host has a printer named "SSH Remote". A document printed to it becomes a PDF in a folder
   of the user who printed it (`print::spool_dir`).
2. The agent of a viewer that prints (`ToAgent::Prints`, sent when `ToClient::Can` has
   `CAN_PRINT`) takes each new job by renaming it, so another viewer's agent does not, and tells
   its viewer (`ToClient::PrintJob`: its number, name, size and path there).
3. The viewer asks what to do: print it here on the default printer, keep it as a PDF in
   Downloads, or let it go. It fetches the file with the ordinary transfer and answers
   `ToAgent::Printed`, upon which the host deletes it. A job an agent took and never had an answer
   for goes back for another viewer as that agent ends.

Both messages need the Files right.

## The host's printer

| System | Printer | Set up |
|---|---|---|
| Windows 10/11 | Windows' own "Microsoft Print To PDF" driver on a local port that is the named pipe `\\.\pipe\sshremote-print`. Each job opens it once; the agent of a viewer that prints listens there and writes each job to `%LOCALAPPDATA%\SSH Remote\print`, its name from the print queue. With no agent listening, Windows keeps the job and tries again. | With the helper service (`--install-service`), or `remote-access printer on` as an administrator |
| Linux | A CUPS queue `SSH_Remote` whose description makes CUPS turn every job into a PDF, and a backend script (`/usr/lib/cups/backend/sshremote`, run as root) that writes it to `/var/spool/sshremote-print/UID` for the user who printed it | `sudo sshremote remote-access printer on` |
| macOS | None: CUPS's backend folder is on the sealed system volume. | |

`remote-access printer off` takes it away; uninstalling the Windows service does too.

## Printing on the viewer

- Linux and macOS: `lp` to the default printer.
- Windows: the PDF's pages drawn by Windows' own PDF renderer (`Windows.Data.Pdf`), fitted to the
  printable area, onto the default printer through GDI.

## Tests

- `print::tests::a_page_printed_there_prints_here`: a page printed on the host to "SSH Remote"
  goes through a local agent, is fetched, and printed on the default printer (CUPS in a container;
  a Windows Server VM with the Print To PDF driver).
- `gui::remote::actions::a_print_job_is_asked_about_and_kept`: the question, and a job kept in
  Downloads under its own name.
