// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {Test} from "forge-std/Test.sol";
import {SignedContextV1} from "rain.interpreter.interface/interface/IInterpreterCallerV2.sol";
import {HOLO_VAULT_ID} from "src/Constants.sol";
import {SignContext} from "./lib/SignContext.sol";

/// The rehearsals sign coupons with SignContext. This pins it to the coupon
/// bridge-orchestrator signs in signer.rs `a_key_signs_the_coupon_the_claim_order_checks`.
contract TestCouponSignature is Test, SignContext {
    function testSignContextSignsTheCouponTheOrchestratorSigns() external pure {
        uint256[] memory context = new uint256[](9);
        context[0] = uint256(uint160(0x1111111111111111111111111111111111111111));
        context[1] = 1.5e18;
        context[2] = 1_750_000_000 + 7 days;
        context[3] = 0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9;
        context[4] = uint256(uint160(0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6));
        context[5] = uint256(uint160(0xfca89cD12Ba1346b1ac570ed988AB43b812733fe));
        context[6] = uint256(uint160(0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749));
        context[7] = HOLO_VAULT_ID;
        context[8] = 8496889498503184870230947373316602526857109467185425766089433217409414560631;

        SignedContextV1 memory coupon =
            signContext(0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a, context);

        assertEq(coupon.signer, 0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC);
        assertEq(
            coupon.signature,
            hex"5700e22974b0b133fd972f920b36e055e35ab5e83c08db389845451c5532232846d64b38b077e85d8c9e311fd7d22b15af7a78f58b5cb11089b9b07b429bade11c"
        );
    }
}
