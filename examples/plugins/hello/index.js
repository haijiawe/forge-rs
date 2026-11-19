#!/usr/bin/env node
// hello — the smallest complete plugin, and the model for any new one.
//
// Copy this directory, rename the plugin in plugin.json, and change callTool.
// Everything else is boilerplate the protocol requires; the three rules below
// are the ones that decide whether a plugin works at all.
//
//  1. stdout carries protocol frames and nothing else. A `console.log` becomes a
//     malformed frame and the host refuses the reply. Use stderr for diagnostics.
//  2. Every reply echoes the request's id exactly. The host rejects a reply whose
//     id differs, because a plugin that mixed up two replies would hand one
//     call's answer to another.
//  3. A tool that ran and refused answers with `isError: true` — a *result*. A
//     JSON-RPC error means the call was never attempted. Those are different
//     facts, reported to different people (the model vs the operator).
'use strict';

const PROTOCOL_VERSION = 1;

// The tools this plugin offers. `schema` is JSON Schema over the call's
// `arguments` object.
const TOOLS = [
  {
    name: 'greet',
    description: 'Greet someone by name. Example: {"name":"world"}',
    schema: {
      type: 'object',
      properties: { name: { type: 'string', description: 'Who to greet.' } },
      required: ['name'],
      additionalProperties: false,
    },
  },
];

// The JSON-RPC numbers PROTOCOL.md documents. Using them means a person can grep
// the number they saw in a host error and find the cause.
const INVALID_PARAMS = -32602;
const METHOD_NOT_FOUND = -32601;
const PARSE_ERROR = -32700;
const INTERNAL_ERROR = -32603;
const REFUSED = -32000;

function write(message) {
  process.stdout.write(JSON.stringify(message) + '\n');
}

function reply(id, result) {
  write({ jsonrpc: '2.0', id: id, result: result });
}

function fail(id, code, message) {
  write({ jsonrpc: '2.0', id: id, error: { code: code, message: message } });
}

// One branch per tool. Return null for a name this plugin does not have: asking
// for something unadvertised is a caller bug (`-32602`), not a tool failure.
function callTool(name, args) {
  const a = args && typeof args === 'object' ? args : {};
  if (name === 'greet') {
    const who = a.name;
    if (typeof who !== 'string' || who === '') {
      return { output: 'greet needs arguments.name to be a non-empty string', isError: true };
    }
    return { output: 'hello, ' + who, isError: false };
  }
  return null;
}

function handle(message) {
  const hasId = Object.prototype.hasOwnProperty.call(message, 'id') && message.id !== null;
  switch (message.method) {
    case 'initialize': {
      const params = message.params;
      if (!params || params.protocolVersion !== PROTOCOL_VERSION) {
        return fail(message.id, REFUSED, 'this plugin speaks protocol version ' + PROTOCOL_VERSION);
      }
      return reply(message.id, {
        protocolVersion: PROTOCOL_VERSION,
        name: 'hello',
        version: '1.0.0',
        // Capabilities are how the host knows what not to ask. Claim only what
        // you implement.
        capabilities: { tools: true, prompts: false, hooks: [] },
      });
    }
    case 'tools/list':
      return reply(message.id, { tools: TOOLS });
    case 'tools/call': {
      const params = message.params || {};
      if (typeof params.name !== 'string' || params.name === '') {
        return fail(message.id, INVALID_PARAMS, 'tools/call needs a tool name');
      }
      const result = callTool(params.name, params.arguments);
      if (result === null) {
        return fail(message.id, INVALID_PARAMS, 'no tool named ' + JSON.stringify(params.name));
      }
      return reply(message.id, result);
    }
    case 'initialized':
      // The host's acknowledgement of the handshake. No reply by definition.
      return;
    case 'shutdown':
      // Answer first, then exit from the write callback: `process.exit()` right
      // after a write to a pipe can truncate it, and a plugin that vanished
      // mid-reply looks like a crash in the host's log.
      process.stdout.write(
        JSON.stringify({ jsonrpc: '2.0', id: message.id, result: {} }) + '\n',
        function () {
          process.exit(0);
        }
      );
      return;
    default:
      if (hasId) {
        return fail(message.id, METHOD_NOT_FOUND, 'no method named ' + JSON.stringify(message.method));
      }
      // An unknown notification is ignorable by the JSON-RPC contract.
      return;
  }
}

// Newline-delimited framing: one frame per line, so splitting on '\n' is the
// whole parser.
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', function (chunk) {
  buffer += chunk;
  let index = buffer.indexOf('\n');
  while (index >= 0) {
    const line = buffer.slice(0, index).trim();
    buffer = buffer.slice(index + 1);
    index = buffer.indexOf('\n');
    if (line === '') continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      // No id can be recovered from a line that did not parse; null is what the
      // protocol allows for a parse error.
      fail(null, PARSE_ERROR, 'the line is not JSON: ' + error.message);
      continue;
    }
    try {
      handle(message);
    } catch (error) {
      // A thrown handler is a bug in this file. Say so on stderr and still
      // answer, or the caller waits until its deadline for nothing.
      process.stderr.write('hello: handler threw: ' + (error && error.stack) + '\n');
      if (Object.prototype.hasOwnProperty.call(message, 'id')) {
        fail(message.id, INTERNAL_ERROR, 'the plugin failed inside ' + String(message.method));
      }
    }
  }
});

// EOF on stdin is the host saying "stop": exit rather than linger.
process.stdin.on('end', function () {
  process.exit(0);
});