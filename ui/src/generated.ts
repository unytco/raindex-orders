// Contract ABIs for HOT Bridge
// Generated from Raindex orderbook contracts

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// Orderbook
//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/**
 * [__View Contract on Sepolia Etherscan__](https://sepolia.etherscan.io/address/0xfca89cD12Ba1346b1ac570ed988AB43b812733fe)
 */
export const orderbookAbi = [
  {
    type: 'error',
    inputs: [{ name: 'result', internalType: 'bytes32', type: 'bytes32' }],
    name: 'FlashLenderCallbackFailed',
  },
  {
    type: 'error',
    inputs: [{ name: 'i', internalType: 'uint256', type: 'uint256' }],
    name: 'InvalidSignature',
  },
  {
    type: 'error',
    inputs: [
      { name: 'minimumInput', internalType: 'uint256', type: 'uint256' },
      { name: 'input', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'MinimumInput',
  },
  { type: 'error', inputs: [], name: 'NoOrders' },
  {
    type: 'error',
    inputs: [
      { name: 'sender', internalType: 'address', type: 'address' },
      { name: 'owner', internalType: 'address', type: 'address' },
    ],
    name: 'NotOrderOwner',
  },
  {
    type: 'error',
    inputs: [{ name: 'unmeta', internalType: 'bytes', type: 'bytes' }],
    name: 'NotRainMetaV1',
  },
  { type: 'error', inputs: [], name: 'OrderNoHandleIO' },
  { type: 'error', inputs: [], name: 'OrderNoInputs' },
  { type: 'error', inputs: [], name: 'OrderNoOutputs' },
  { type: 'error', inputs: [], name: 'OrderNoSources' },
  {
    type: 'error',
    inputs: [{ name: 'owner', internalType: 'address', type: 'address' }],
    name: 'SameOwner',
  },
  {
    type: 'error',
    inputs: [
      { name: 'bytecode', internalType: 'bytes', type: 'bytes' },
      { name: 'sourceIndex', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'SourceIndexOutOfBounds',
  },
  {
    type: 'error',
    inputs: [
      { name: 'aliceTokenDecimals', internalType: 'uint8', type: 'uint8' },
      { name: 'bobTokenDecimals', internalType: 'uint8', type: 'uint8' },
    ],
    name: 'TokenDecimalsMismatch',
  },
  {
    type: 'error',
    inputs: [
      { name: 'aliceToken', internalType: 'address', type: 'address' },
      { name: 'bobToken', internalType: 'address', type: 'address' },
    ],
    name: 'TokenMismatch',
  },
  {
    type: 'error',
    inputs: [{ name: 'inputs', internalType: 'uint256', type: 'uint256' }],
    name: 'UnsupportedCalculateInputs',
  },
  {
    type: 'error',
    inputs: [{ name: 'outputs', internalType: 'uint256', type: 'uint256' }],
    name: 'UnsupportedCalculateOutputs',
  },
  {
    type: 'error',
    inputs: [{ name: 'inputs', internalType: 'uint256', type: 'uint256' }],
    name: 'UnsupportedHandleInputs',
  },
  {
    type: 'error',
    inputs: [
      { name: 'sender', internalType: 'address', type: 'address' },
      { name: 'token', internalType: 'address', type: 'address' },
      { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'ZeroDepositAmount',
  },
  { type: 'error', inputs: [], name: 'ZeroMaximumInput' },
  {
    type: 'error',
    inputs: [
      { name: 'sender', internalType: 'address', type: 'address' },
      { name: 'token', internalType: 'address', type: 'address' },
      { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'ZeroWithdrawTargetAmount',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'expressionDeployer',
        internalType: 'contract IExpressionDeployerV3',
        type: 'address',
        indexed: false,
      },
      {
        name: 'order',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
        indexed: false,
      },
      {
        name: 'orderHash',
        internalType: 'bytes32',
        type: 'bytes32',
        indexed: false,
      },
    ],
    name: 'AddOrder',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'clearStateChange',
        internalType: 'struct ClearStateChange',
        type: 'tuple',
        components: [
          { name: 'aliceOutput', internalType: 'uint256', type: 'uint256' },
          { name: 'bobOutput', internalType: 'uint256', type: 'uint256' },
          { name: 'aliceInput', internalType: 'uint256', type: 'uint256' },
          { name: 'bobInput', internalType: 'uint256', type: 'uint256' },
        ],
        indexed: false,
      },
    ],
    name: 'AfterClear',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'alice',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
        indexed: false,
      },
      {
        name: 'bob',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
        indexed: false,
      },
      {
        name: 'clearConfig',
        internalType: 'struct ClearConfig',
        type: 'tuple',
        components: [
          {
            name: 'aliceInputIOIndex',
            internalType: 'uint256',
            type: 'uint256',
          },
          {
            name: 'aliceOutputIOIndex',
            internalType: 'uint256',
            type: 'uint256',
          },
          { name: 'bobInputIOIndex', internalType: 'uint256', type: 'uint256' },
          {
            name: 'bobOutputIOIndex',
            internalType: 'uint256',
            type: 'uint256',
          },
          {
            name: 'aliceBountyVaultId',
            internalType: 'uint256',
            type: 'uint256',
          },
          {
            name: 'bobBountyVaultId',
            internalType: 'uint256',
            type: 'uint256',
          },
        ],
        indexed: false,
      },
    ],
    name: 'Clear',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'context',
        internalType: 'uint256[][]',
        type: 'uint256[][]',
        indexed: false,
      },
    ],
    name: 'Context',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'token',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'vaultId',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
      {
        name: 'amount',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
    ],
    name: 'Deposit',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'subject',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
      { name: 'meta', internalType: 'bytes', type: 'bytes', indexed: false },
    ],
    name: 'MetaV1',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'owner',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'orderHash',
        internalType: 'bytes32',
        type: 'bytes32',
        indexed: false,
      },
    ],
    name: 'OrderExceedsMaxRatio',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'owner',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'orderHash',
        internalType: 'bytes32',
        type: 'bytes32',
        indexed: false,
      },
    ],
    name: 'OrderNotFound',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'owner',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'orderHash',
        internalType: 'bytes32',
        type: 'bytes32',
        indexed: false,
      },
    ],
    name: 'OrderZeroAmount',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'order',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
        indexed: false,
      },
      {
        name: 'orderHash',
        internalType: 'bytes32',
        type: 'bytes32',
        indexed: false,
      },
    ],
    name: 'RemoveOrder',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'config',
        internalType: 'struct TakeOrderConfigV2',
        type: 'tuple',
        components: [
          {
            name: 'order',
            internalType: 'struct OrderV2',
            type: 'tuple',
            components: [
              { name: 'owner', internalType: 'address', type: 'address' },
              { name: 'handleIO', internalType: 'bool', type: 'bool' },
              {
                name: 'evaluable',
                internalType: 'struct EvaluableV2',
                type: 'tuple',
                components: [
                  {
                    name: 'interpreter',
                    internalType: 'contract IInterpreterV2',
                    type: 'address',
                  },
                  {
                    name: 'store',
                    internalType: 'contract IInterpreterStoreV2',
                    type: 'address',
                  },
                  {
                    name: 'expression',
                    internalType: 'address',
                    type: 'address',
                  },
                ],
              },
              {
                name: 'validInputs',
                internalType: 'struct IO[]',
                type: 'tuple[]',
                components: [
                  { name: 'token', internalType: 'address', type: 'address' },
                  { name: 'decimals', internalType: 'uint8', type: 'uint8' },
                  { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
                ],
              },
              {
                name: 'validOutputs',
                internalType: 'struct IO[]',
                type: 'tuple[]',
                components: [
                  { name: 'token', internalType: 'address', type: 'address' },
                  { name: 'decimals', internalType: 'uint8', type: 'uint8' },
                  { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
                ],
              },
            ],
          },
          { name: 'inputIOIndex', internalType: 'uint256', type: 'uint256' },
          { name: 'outputIOIndex', internalType: 'uint256', type: 'uint256' },
          {
            name: 'signedContext',
            internalType: 'struct SignedContextV1[]',
            type: 'tuple[]',
            components: [
              { name: 'signer', internalType: 'address', type: 'address' },
              { name: 'context', internalType: 'uint256[]', type: 'uint256[]' },
              { name: 'signature', internalType: 'bytes', type: 'bytes' },
            ],
          },
        ],
        indexed: false,
      },
      {
        name: 'input',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
      {
        name: 'output',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
    ],
    name: 'TakeOrder',
  },
  {
    type: 'event',
    anonymous: false,
    inputs: [
      {
        name: 'sender',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'token',
        internalType: 'address',
        type: 'address',
        indexed: false,
      },
      {
        name: 'vaultId',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
      {
        name: 'targetAmount',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
      {
        name: 'amount',
        internalType: 'uint256',
        type: 'uint256',
        indexed: false,
      },
    ],
    name: 'Withdraw',
  },
  {
    type: 'function',
    inputs: [
      {
        name: 'config',
        internalType: 'struct OrderConfigV2',
        type: 'tuple',
        components: [
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'evaluableConfig',
            internalType: 'struct EvaluableConfigV3',
            type: 'tuple',
            components: [
              {
                name: 'deployer',
                internalType: 'contract IExpressionDeployerV3',
                type: 'address',
              },
              { name: 'bytecode', internalType: 'bytes', type: 'bytes' },
              {
                name: 'constants',
                internalType: 'uint256[]',
                type: 'uint256[]',
              },
            ],
          },
          { name: 'meta', internalType: 'bytes', type: 'bytes' },
        ],
      },
    ],
    name: 'addOrder',
    outputs: [{ name: 'stateChanged', internalType: 'bool', type: 'bool' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [
      {
        name: 'aliceOrder',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
      },
      {
        name: 'bobOrder',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
      },
      {
        name: 'clearConfig',
        internalType: 'struct ClearConfig',
        type: 'tuple',
        components: [
          {
            name: 'aliceInputIOIndex',
            internalType: 'uint256',
            type: 'uint256',
          },
          {
            name: 'aliceOutputIOIndex',
            internalType: 'uint256',
            type: 'uint256',
          },
          { name: 'bobInputIOIndex', internalType: 'uint256', type: 'uint256' },
          {
            name: 'bobOutputIOIndex',
            internalType: 'uint256',
            type: 'uint256',
          },
          {
            name: 'aliceBountyVaultId',
            internalType: 'uint256',
            type: 'uint256',
          },
          {
            name: 'bobBountyVaultId',
            internalType: 'uint256',
            type: 'uint256',
          },
        ],
      },
      {
        name: 'aliceSignedContext',
        internalType: 'struct SignedContextV1[]',
        type: 'tuple[]',
        components: [
          { name: 'signer', internalType: 'address', type: 'address' },
          { name: 'context', internalType: 'uint256[]', type: 'uint256[]' },
          { name: 'signature', internalType: 'bytes', type: 'bytes' },
        ],
      },
      {
        name: 'bobSignedContext',
        internalType: 'struct SignedContextV1[]',
        type: 'tuple[]',
        components: [
          { name: 'signer', internalType: 'address', type: 'address' },
          { name: 'context', internalType: 'uint256[]', type: 'uint256[]' },
          { name: 'signature', internalType: 'bytes', type: 'bytes' },
        ],
      },
    ],
    name: 'clear',
    outputs: [],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [
      { name: 'token', internalType: 'address', type: 'address' },
      { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
      { name: 'amount', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'deposit',
    outputs: [],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [
      { name: '', internalType: 'address', type: 'address' },
      { name: '', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'flashFee',
    outputs: [{ name: '', internalType: 'uint256', type: 'uint256' }],
    stateMutability: 'pure',
  },
  {
    type: 'function',
    inputs: [
      {
        name: 'receiver',
        internalType: 'contract IERC3156FlashBorrower',
        type: 'address',
      },
      { name: 'token', internalType: 'address', type: 'address' },
      { name: 'amount', internalType: 'uint256', type: 'uint256' },
      { name: 'data', internalType: 'bytes', type: 'bytes' },
    ],
    name: 'flashLoan',
    outputs: [{ name: '', internalType: 'bool', type: 'bool' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [{ name: 'token', internalType: 'address', type: 'address' }],
    name: 'maxFlashLoan',
    outputs: [{ name: '', internalType: 'uint256', type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [{ name: 'data', internalType: 'bytes[]', type: 'bytes[]' }],
    name: 'multicall',
    outputs: [{ name: 'results', internalType: 'bytes[]', type: 'bytes[]' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [{ name: 'orderHash', internalType: 'bytes32', type: 'bytes32' }],
    name: 'orderExists',
    outputs: [{ name: '', internalType: 'bool', type: 'bool' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [
      {
        name: 'order',
        internalType: 'struct OrderV2',
        type: 'tuple',
        components: [
          { name: 'owner', internalType: 'address', type: 'address' },
          { name: 'handleIO', internalType: 'bool', type: 'bool' },
          {
            name: 'evaluable',
            internalType: 'struct EvaluableV2',
            type: 'tuple',
            components: [
              {
                name: 'interpreter',
                internalType: 'contract IInterpreterV2',
                type: 'address',
              },
              {
                name: 'store',
                internalType: 'contract IInterpreterStoreV2',
                type: 'address',
              },
              { name: 'expression', internalType: 'address', type: 'address' },
            ],
          },
          {
            name: 'validInputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
          {
            name: 'validOutputs',
            internalType: 'struct IO[]',
            type: 'tuple[]',
            components: [
              { name: 'token', internalType: 'address', type: 'address' },
              { name: 'decimals', internalType: 'uint8', type: 'uint8' },
              { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
            ],
          },
        ],
      },
    ],
    name: 'removeOrder',
    outputs: [{ name: 'stateChanged', internalType: 'bool', type: 'bool' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [{ name: 'interfaceId', internalType: 'bytes4', type: 'bytes4' }],
    name: 'supportsInterface',
    outputs: [{ name: '', internalType: 'bool', type: 'bool' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [
      {
        name: 'config',
        internalType: 'struct TakeOrdersConfigV2',
        type: 'tuple',
        components: [
          { name: 'minimumInput', internalType: 'uint256', type: 'uint256' },
          { name: 'maximumInput', internalType: 'uint256', type: 'uint256' },
          { name: 'maximumIORatio', internalType: 'uint256', type: 'uint256' },
          {
            name: 'orders',
            internalType: 'struct TakeOrderConfigV2[]',
            type: 'tuple[]',
            components: [
              {
                name: 'order',
                internalType: 'struct OrderV2',
                type: 'tuple',
                components: [
                  { name: 'owner', internalType: 'address', type: 'address' },
                  { name: 'handleIO', internalType: 'bool', type: 'bool' },
                  {
                    name: 'evaluable',
                    internalType: 'struct EvaluableV2',
                    type: 'tuple',
                    components: [
                      {
                        name: 'interpreter',
                        internalType: 'contract IInterpreterV2',
                        type: 'address',
                      },
                      {
                        name: 'store',
                        internalType: 'contract IInterpreterStoreV2',
                        type: 'address',
                      },
                      {
                        name: 'expression',
                        internalType: 'address',
                        type: 'address',
                      },
                    ],
                  },
                  {
                    name: 'validInputs',
                    internalType: 'struct IO[]',
                    type: 'tuple[]',
                    components: [
                      {
                        name: 'token',
                        internalType: 'address',
                        type: 'address',
                      },
                      {
                        name: 'decimals',
                        internalType: 'uint8',
                        type: 'uint8',
                      },
                      {
                        name: 'vaultId',
                        internalType: 'uint256',
                        type: 'uint256',
                      },
                    ],
                  },
                  {
                    name: 'validOutputs',
                    internalType: 'struct IO[]',
                    type: 'tuple[]',
                    components: [
                      {
                        name: 'token',
                        internalType: 'address',
                        type: 'address',
                      },
                      {
                        name: 'decimals',
                        internalType: 'uint8',
                        type: 'uint8',
                      },
                      {
                        name: 'vaultId',
                        internalType: 'uint256',
                        type: 'uint256',
                      },
                    ],
                  },
                ],
              },
              {
                name: 'inputIOIndex',
                internalType: 'uint256',
                type: 'uint256',
              },
              {
                name: 'outputIOIndex',
                internalType: 'uint256',
                type: 'uint256',
              },
              {
                name: 'signedContext',
                internalType: 'struct SignedContextV1[]',
                type: 'tuple[]',
                components: [
                  { name: 'signer', internalType: 'address', type: 'address' },
                  {
                    name: 'context',
                    internalType: 'uint256[]',
                    type: 'uint256[]',
                  },
                  { name: 'signature', internalType: 'bytes', type: 'bytes' },
                ],
              },
            ],
          },
          { name: 'data', internalType: 'bytes', type: 'bytes' },
        ],
      },
    ],
    name: 'takeOrders',
    outputs: [
      { name: 'totalTakerInput', internalType: 'uint256', type: 'uint256' },
      { name: 'totalTakerOutput', internalType: 'uint256', type: 'uint256' },
    ],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [
      { name: 'owner', internalType: 'address', type: 'address' },
      { name: 'token', internalType: 'address', type: 'address' },
      { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'vaultBalance',
    outputs: [{ name: '', internalType: 'uint256', type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [
      { name: 'token', internalType: 'address', type: 'address' },
      { name: 'vaultId', internalType: 'uint256', type: 'uint256' },
      { name: 'targetAmount', internalType: 'uint256', type: 'uint256' },
    ],
    name: 'withdraw',
    outputs: [],
    stateMutability: 'nonpayable',
  },
] as const

/**
 * [__View Contract on Sepolia Etherscan__](https://sepolia.etherscan.io/address/0xfca89cD12Ba1346b1ac570ed988AB43b812733fe)
 */
export const orderbookAddress = {
  11155111: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe',
} as const

/**
 * [__View Contract on Sepolia Etherscan__](https://sepolia.etherscan.io/address/0xfca89cD12Ba1346b1ac570ed988AB43b812733fe)
 */
export const orderbookConfig = {
  address: orderbookAddress,
  abi: orderbookAbi,
} as const

//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// erc20
//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

export const erc20Abi = [
  {
    type: 'event',
    inputs: [
      { name: 'owner', type: 'address', indexed: true },
      { name: 'spender', type: 'address', indexed: true },
      { name: 'value', type: 'uint256', indexed: false },
    ],
    name: 'Approval',
  },
  {
    type: 'event',
    inputs: [
      { name: 'from', type: 'address', indexed: true },
      { name: 'to', type: 'address', indexed: true },
      { name: 'value', type: 'uint256', indexed: false },
    ],
    name: 'Transfer',
  },
  {
    type: 'function',
    inputs: [
      { name: 'owner', type: 'address' },
      { name: 'spender', type: 'address' },
    ],
    name: 'allowance',
    outputs: [{ type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [
      { name: 'spender', type: 'address' },
      { name: 'amount', type: 'uint256' },
    ],
    name: 'approve',
    outputs: [{ type: 'bool' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [{ name: 'account', type: 'address' }],
    name: 'balanceOf',
    outputs: [{ type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [],
    name: 'decimals',
    outputs: [{ type: 'uint8' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [],
    name: 'name',
    outputs: [{ type: 'string' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [],
    name: 'symbol',
    outputs: [{ type: 'string' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [],
    name: 'totalSupply',
    outputs: [{ type: 'uint256' }],
    stateMutability: 'view',
  },
  {
    type: 'function',
    inputs: [
      { name: 'recipient', type: 'address' },
      { name: 'amount', type: 'uint256' },
    ],
    name: 'transfer',
    outputs: [{ type: 'bool' }],
    stateMutability: 'nonpayable',
  },
  {
    type: 'function',
    inputs: [
      { name: 'sender', type: 'address' },
      { name: 'recipient', type: 'address' },
      { name: 'amount', type: 'uint256' },
    ],
    name: 'transferFrom',
    outputs: [{ type: 'bool' }],
    stateMutability: 'nonpayable',
  },
] as const

