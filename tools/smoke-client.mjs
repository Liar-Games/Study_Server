const base = process.argv[1];
const deadline = setTimeout(() => { console.error('shutdown timeout'); process.exit(2); }, 15000);
(async () => {
  for (const path of ['/health/live', '/health/ready']) {
    const response = await fetch(base + path);
    if (response.status !== 200) throw new Error(`${path}: ${response.status}`);
  }
  const socket = new WebSocket(base.replace('http:', 'ws:') + '/ws');
  socket.binaryType = 'arraybuffer';
  socket.onopen = () => {
    const body = new TextEncoder().encode(JSON.stringify({mode:'guest',version:1}));
    const packet = new Uint8Array(body.length + 1); packet[0] = 1; packet.set(body, 1); socket.send(packet);
  };
  socket.onmessage = ({data}) => {
    if (new Uint8Array(data)[0] !== 2) throw new Error('expected Welcome');
    console.log('READY');
  };
  socket.onerror = () => { console.error('websocket error'); process.exit(3); };
  socket.onclose = ({code}) => {
    clearTimeout(deadline);
    console.log(`CLOSED ${code}`);
    process.exit(code === 1006 ? 4 : 0);
  };
})().catch(error => { console.error(error); process.exit(1); });
