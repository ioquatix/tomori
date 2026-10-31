# Releases

## Unreleased

- Release Tomori under the MIT License only, replacing the MIT or Apache-2.0 dual license.
- The headset joins spaces on a Tomori server instead of chatting through the PC bridge: it lists servers found with Bonjour and the USB connection, reconnects to the last server when it finds it again, then lists the server's spaces. It reconnects and rejoins after a dropped connection, says when a space has ended, as after the server restarts, and answers approvals with the right stick and **A**.
- Choose the server and display name with the intent extras `tomori.server` and `tomori.name`, `TOMORI_SERVER=host:port bash scripts/android.sh run` (with `TOMORI_SERVER_PORT` for the port forwarded over USB), or `--server` and `--name` on the desktop.
- Add `--console`, which shows the headset's panel in the terminal for using a server without a headset or a GPU.
- Remove the PC bridge's voice path, spoken replies, voice preview, replay, result cards and spectator publishing from the headset for now.
- Hold **X** in a space to talk to its agent through the server: the headset sends push-to-talk audio over its server connection, shows the words recognized so far and the microphone's state on the panel, and the server transcribes them and queues the text. Lost audio, a dropped connection, nothing heard or a full queue fails visibly instead of sending anything. In `--console`, `x` starts talking through the computer's default microphone and `x` again sends it.
- The speech setup scripts work on macOS, where Whisper runs on the CPU, so `tomori-server --speech-root .` recognizes speech there too.
- `tomori-server` keeps serving after transient accept errors, such as running out of file descriptors, instead of silently leaving its port open with nobody accepting. When serving does end it refuses new clients at once, waits at most 5 seconds for open connections, and exits with a failure status if serving failed. It now logs to standard error, controlled by `RUST_LOG`.
