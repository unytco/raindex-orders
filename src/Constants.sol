// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {IOrderBookV3} from "rain.orderbook.interface/interface/IOrderBookV3.sol";
import {IExpressionDeployerV3} from "rain.interpreter.interface/interface/IExpressionDeployerV3.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";

// ========== MAINNET ==========
IOrderBookV3 constant MAINNET_ORDERBOOK = IOrderBookV3(0xf1224A483ad7F1E9aA46A8CE41229F32d7549A74);
IExpressionDeployerV3 constant MAINNET_DEPLOYER = IExpressionDeployerV3(0x56Fa1748867fD547F3cc6C064B809ab84bc7e9B9);
IERC20 constant MAINNET_HOT = IERC20(0x6c6EE5e31d828De241282B9606C8e98Ea48526E2);
IERC20 constant MAINNET_USDT = IERC20(0xdAC17F958D2ee523a2206206994597C13D831ec7);

// ========== SEPOLIA ==========
IOrderBookV3 constant SEPOLIA_ORDERBOOK = IOrderBookV3(0xfca89cD12Ba1346b1ac570ed988AB43b812733fe);
IExpressionDeployerV3 constant SEPOLIA_DEPLOYER = IExpressionDeployerV3(0xd19581a021f4704ad4eBfF68258e7A0a9DB1CD77);
IERC20 constant SEPOLIA_TROT = IERC20(0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b);
IERC20 constant SEPOLIA_NOOP = IERC20(0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028);

// ========== SHARED ==========
bytes32 constant TEST_SIGNER_KEY = 0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd;
address constant TEST_SIGNER_ADDRESS = 0x8E72b7568738da52ca3DCd9b24E178127A4E7d37;

// Vault ID used by the HoloLockVault (same as in holo-claim.rain)
uint256 constant HOLO_VAULT_ID = 0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b;

// Minimum lock amount (1 token with 18 decimals)
uint256 constant MIN_LOCK_AMOUNT = 1e18;
