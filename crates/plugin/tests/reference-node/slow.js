// A plugin that takes far longer than any sane timeout, to prove the host kills
// it rather than waiting forever.
const readline = require('node:readline');

const reader = readline.createInterface({ input: process.stdin });
reader.on('line', (line) => {
  const text = line.trim();
  if (text === '') {
    return;
  }
  const request = JSON.parse(text);
  if (request.method === 'initialize') {
    process.stdout.write(
      JSON.stringify({
        jsonrpc: '2.0',
        id: request.id,
        result: {
          protocolVersion: 1,
          name: 'slow',
          version: '0.1.0',
          capabilities: { tools: true, prompts: false, hooks: [] },
        },
      }) + '\n'
    );
    return;
  }
  // Deliberately never answers.
  setTimeout(() => {}, 60_000);
});
