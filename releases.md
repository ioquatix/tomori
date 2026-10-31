# Releases

## Unreleased

- Release Tomori under the MIT License only, replacing the MIT or Apache-2.0 dual license.
- The headset joins spaces on a Tomori server instead of chatting through the PC bridge: it lists servers found with Bonjour and the USB connection, reconnects to the last server when it finds it again, then lists the server's spaces. It reconnects and rejoins after a dropped connection, says when a space has ended, as after the server restarts, and answers approvals with the right stick and **A**.
- Choose the server and display name with the intent extras `tomori.server` and `tomori.name`, `TOMORI_SERVER=host:port bash scripts/android.sh run` (with `TOMORI_SERVER_PORT` for the port forwarded over USB), or `--server` and `--name` on the desktop.
- Add `--console`, which shows the headset's panel in the terminal for using a server without a headset or a GPU.
- Remove on-device voice capture, spoken replies, voice preview, replay, result cards and spectator publishing from the headset for now; voice returns through the server.
