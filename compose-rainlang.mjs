#!/usr/bin/env node
// Compose rainlang from .rain file using @rainlanguage/dotrain
import pkg from '@rainlanguage/dotrain';
const { RainDocument, MetaStore } = pkg;
import { readFileSync } from 'fs';

const rainFile = process.argv[2] || 'src/holo-claim.rain';

const content = readFileSync(rainFile, 'utf8');

// Create MetaStore
const metaStore = new MetaStore();

// Sepolia bindings from the .rain file
const rebinds = [
  ["orderbook-subparser", "0xe6A589716d5a72276C08E0e08bc941a28005e55A"],
  ["valid-signer", "0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"]
];

// Compose with rebindings
const rainlang = await RainDocument.composeText(
  content,
  ["calculate-io", "handle-io"],
  metaStore,
  rebinds
);

console.log(rainlang);
