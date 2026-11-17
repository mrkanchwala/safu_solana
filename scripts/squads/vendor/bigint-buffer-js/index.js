'use strict';
// Pure-JS drop-in for bigint-buffer 1.1.5: the same code its dist/node.js runs when the native addon
// is absent. The advisory (GHSA-3gc7-fjrx-p6mg, buffer overflow) is in the native addon only.
Object.defineProperty(exports, '__esModule', { value: true });

function toBigIntLE(buf) {
  const reversed = Buffer.from(buf);
  reversed.reverse();
  const hex = reversed.toString('hex');
  return hex.length === 0 ? BigInt(0) : BigInt(`0x${hex}`);
}

function toBigIntBE(buf) {
  const hex = Buffer.from(buf).toString('hex');
  return hex.length === 0 ? BigInt(0) : BigInt(`0x${hex}`);
}

function toBufferLE(num, width) {
  const buffer = toBufferBE(num, width);
  buffer.reverse();
  return buffer;
}

function toBufferBE(num, width) {
  const hex = num.toString(16);
  return Buffer.from(hex.padStart(width * 2, '0').slice(0, width * 2), 'hex');
}

exports.toBigIntLE = toBigIntLE;
exports.toBigIntBE = toBigIntBE;
exports.toBufferLE = toBufferLE;
exports.toBufferBE = toBufferBE;
