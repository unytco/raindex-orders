// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {IExpressionDeployerV3} from "rain.interpreter.interface/interface/IExpressionDeployerV3.sol";
import {IInterpreterV2} from "rain.interpreter.interface/interface/IInterpreterV2.sol";
import {IInterpreterStoreV2} from "rain.interpreter.interface/interface/IInterpreterStoreV2.sol";

/// An expression deployer that is not the network's, though the expressions it
/// returns are the real deployer's, byte for byte.
contract ForwardingDeployer {
    IExpressionDeployerV3 private immutable deployer;

    constructor(IExpressionDeployerV3 real) {
        deployer = real;
    }

    function deployExpression2(bytes calldata bytecode, uint256[] calldata constants)
        external
        returns (IInterpreterV2, IInterpreterStoreV2, address, bytes memory)
    {
        return deployer.deployExpression2(bytecode, constants);
    }
}
