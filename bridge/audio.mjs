// OBS-compatible live WAV. Two bounded real-time sources are mixed at 16 kHz;
// late chunks are dropped, not replayed after a disconnect or microphone mute.
export const SAMPLE_RATE = 16000;
export const SAMPLES_PER_TICK = 800;
export const BYTES_PER_TICK = SAMPLES_PER_TICK * 2;

export function wavHeader() {
  const b = Buffer.alloc(44);
  b.write("RIFF", 0); b.writeUInt32LE(0xffffffff, 4); b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(SAMPLE_RATE, 24); b.writeUInt32LE(SAMPLE_RATE * 2, 28);
  b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34); b.write("data", 36); b.writeUInt32LE(0xffffffff, 40);
  return b;
}

export class AudioMixer {
  constructor() {
    this.listeners = new Map();
    this.sources = { mic: Buffer.alloc(0), agent: Buffer.alloc(0) };
    this.timer = setInterval(() => this.tick(), 50);
    this.timer.unref();
  }
  subscribe(response, source = "mix") {
    response.writeHead(200, { "content-type": "audio/wav", "cache-control": "no-store", "x-content-type-options": "nosniff" });
    response.write(wavHeader());
    this.listeners.set(response, source);
    response.on("close", () => this.listeners.delete(response));
  }
  push(source, bytes) {
    if (!Object.hasOwn(this.sources, source) || !bytes.length || bytes.length > 9600 || bytes.length % 2) throw new Error("Invalid audio packet");
    if (!this.listeners.size) return;
    const data = Buffer.concat([this.sources[source], bytes]);
    // Keep at most 150 ms. Preserve whole samples when dropping old data.
    this.sources[source] = data.subarray(Math.max(0, data.length - BYTES_PER_TICK * 3));
  }
  tick() {
    if (!this.listeners.size) { this.sources.mic = Buffer.alloc(0); this.sources.agent = Buffer.alloc(0); return; }
    const output = Buffer.alloc(BYTES_PER_TICK);
    const mic = this.sources.mic;
    const agent = this.sources.agent;
    const micOutput = Buffer.alloc(BYTES_PER_TICK);
    const agentOutput = Buffer.alloc(BYTES_PER_TICK);
    mic.copy(micOutput, 0, 0, Math.min(mic.length, BYTES_PER_TICK));
    agent.copy(agentOutput, 0, 0, Math.min(agent.length, BYTES_PER_TICK));
    for (let i = 0; i < BYTES_PER_TICK; i += 2) {
      const sample = (i + 1 < mic.length ? mic.readInt16LE(i) : 0) + (i + 1 < agent.length ? agent.readInt16LE(i) : 0);
      output.writeInt16LE(Math.max(-32768, Math.min(32767, sample)), i);
    }
    this.sources.mic = mic.subarray(BYTES_PER_TICK);
    this.sources.agent = agent.subarray(BYTES_PER_TICK);
    for (const [listener, source] of this.listeners) {
      const buffer = source === "mic" ? micOutput : source === "agent" ? agentOutput : output;
      if (!listener.write(buffer)) { listener.destroy(); this.listeners.delete(listener); }
    }
  }
}
