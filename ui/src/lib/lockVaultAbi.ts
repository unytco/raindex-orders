export const holoLockVaultAbi = [
  {
    type: 'event',
    name: 'Lock',
    inputs: [
      { name: 'sender', type: 'address', indexed: true },
      { name: 'amount', type: 'uint256', indexed: false },
      { name: 'holochainAgent', type: 'bytes32', indexed: true },
      { name: 'lockId', type: 'uint256', indexed: false },
    ],
  },
  {
    type: 'event',
    name: 'AdminWithdraw',
    inputs: [
      { name: 'admin', type: 'address', indexed: true },
      { name: 'amount', type: 'uint256', indexed: false },
      { name: 'to', type: 'address', indexed: true },
    ],
  },
  {
    type: 'event',
    name: 'AdminChanged',
    inputs: [
      { name: 'oldAdmin', type: 'address', indexed: true },
      { name: 'newAdmin', type: 'address', indexed: true },
    ],
  },
  {
    type: 'function',
    name: 'lock',
    inputs: [
      { name: 'amount', type: 'uint256' },
      { name: 'holochainAgent', type: 'bytes32' },
    ],
    outputs: [{ name: 'lockId', type: 'uint256' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    name: 'vaultBalance',
    inputs: [],
    outputs: [{ name: 'balance', type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    name: 'lockNonce',
    inputs: [],
    outputs: [{ type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    name: 'token',
    inputs: [],
    outputs: [{ type: 'address' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    name: 'orderbook',
    inputs: [],
    outputs: [{ type: 'address' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    name: 'vaultId',
    inputs: [],
    outputs: [{ type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    name: 'admin',
    inputs: [],
    outputs: [{ type: 'address' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    name: 'minLockAmount',
    inputs: [],
    outputs: [{ type: 'uint256' }],
    stateMutability: 'view',
  },
] as const
