// SPDX-License-Identifier: CAL
pragma solidity =0.8.19;

import {console2, Test, Vm} from "forge-std/Test.sol";
import {HoloLockVault} from "src/HoloLockVault.sol";
import {MockHOT} from "src/test/MockHOT.sol";
import {MockOrderBook} from "src/test/MockOrderBook.sol";
import {IERC20} from "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import {IOrderBookV3} from "rain.orderbook.interface/interface/IOrderBookV3.sol";

contract TestHoloLockVault is Test {
    HoloLockVault public lockVault;
    MockHOT public token;
    MockOrderBook public orderbook;

    address public admin;
    address public user1;
    address public user2;

    uint256 public constant VAULT_ID = 0x1234;
    uint256 public constant MIN_LOCK_AMOUNT = 1e18; // 1 token minimum

    // Example Holochain agent pub keys (32 bytes)
    bytes32 public constant AGENT_1 = keccak256("holochain-agent-1");
    bytes32 public constant AGENT_2 = keccak256("holochain-agent-2");

    event Lock(
        address indexed sender,
        uint256 amount,
        bytes32 indexed holochainAgent,
        uint256 lockId
    );

    event AdminWithdraw(address indexed admin, uint256 amount, address indexed to);
    event AdminChanged(address indexed oldAdmin, address indexed newAdmin);

    function setUp() public {
        admin = makeAddr("admin");
        user1 = makeAddr("user1");
        user2 = makeAddr("user2");

        // Deploy mock contracts
        token = new MockHOT();
        orderbook = new MockOrderBook();

        // Deploy HoloLockVault
        lockVault = new HoloLockVault(
            address(token),
            address(orderbook),
            VAULT_ID,
            admin,
            MIN_LOCK_AMOUNT
        );

        // Mint tokens to users
        token.mint(user1, 1000e18);
        token.mint(user2, 1000e18);
    }

    // ========== Constructor Tests ==========

    function testConstructorSetsImmutables() public view {
        assertEq(address(lockVault.token()), address(token));
        assertEq(address(lockVault.orderbook()), address(orderbook));
        assertEq(lockVault.vaultId(), VAULT_ID);
        assertEq(lockVault.admin(), admin);
        assertEq(lockVault.minLockAmount(), MIN_LOCK_AMOUNT);
    }

    function testConstructorApprovesOrderbook() public view {
        uint256 allowance = token.allowance(address(lockVault), address(orderbook));
        assertEq(allowance, type(uint256).max);
    }

    function testConstructorRevertsZeroToken() public {
        vm.expectRevert("HoloLockVault: zero token address");
        new HoloLockVault(
            address(0),
            address(orderbook),
            VAULT_ID,
            admin,
            MIN_LOCK_AMOUNT
        );
    }

    function testConstructorRevertsZeroOrderbook() public {
        vm.expectRevert("HoloLockVault: zero orderbook address");
        new HoloLockVault(
            address(token),
            address(0),
            VAULT_ID,
            admin,
            MIN_LOCK_AMOUNT
        );
    }

    function testConstructorRevertsZeroAdmin() public {
        vm.expectRevert("HoloLockVault: zero admin address");
        new HoloLockVault(
            address(token),
            address(orderbook),
            VAULT_ID,
            address(0),
            MIN_LOCK_AMOUNT
        );
    }

    // ========== Lock Tests ==========

    function testLockSuccess() public {
        uint256 amount = 100e18;

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);

        vm.expectEmit(true, true, true, true);
        emit Lock(user1, amount, AGENT_1, 0);

        uint256 lockId = lockVault.lock(amount, AGENT_1);
        vm.stopPrank();

        assertEq(lockId, 0);
        assertEq(token.balanceOf(user1), 900e18);
        assertEq(lockVault.vaultBalance(), amount);
    }

    function testLockMultipleTimes() public {
        uint256 amount1 = 100e18;
        uint256 amount2 = 200e18;

        vm.startPrank(user1);
        token.approve(address(lockVault), amount1 + amount2);

        uint256 lockId1 = lockVault.lock(amount1, AGENT_1);
        uint256 lockId2 = lockVault.lock(amount2, AGENT_1);
        vm.stopPrank();

        assertEq(lockId1, 0);
        assertEq(lockId2, 1);
        assertEq(lockVault.lockNonce(), 2);
        assertEq(lockVault.vaultBalance(), amount1 + amount2);
    }

    function testLockFromMultipleUsers() public {
        uint256 amount = 100e18;

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);
        uint256 lockId1 = lockVault.lock(amount, AGENT_1);
        vm.stopPrank();

        vm.startPrank(user2);
        token.approve(address(lockVault), amount);
        uint256 lockId2 = lockVault.lock(amount, AGENT_2);
        vm.stopPrank();

        assertEq(lockId1, 0);
        assertEq(lockId2, 1);
        assertEq(lockVault.vaultBalance(), 2 * amount);
    }

    function testLockRevertsBelowMinimum() public {
        uint256 amount = MIN_LOCK_AMOUNT - 1;

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);

        vm.expectRevert("HoloLockVault: amount below minimum");
        lockVault.lock(amount, AGENT_1);
        vm.stopPrank();
    }

    function testLockRevertsInvalidAgent() public {
        uint256 amount = 100e18;

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);

        vm.expectRevert("HoloLockVault: invalid holochain agent");
        lockVault.lock(amount, bytes32(0));
        vm.stopPrank();
    }

    function testLockRevertsInsufficientAllowance() public {
        uint256 amount = 100e18;

        vm.startPrank(user1);
        // Don't approve

        vm.expectRevert();
        lockVault.lock(amount, AGENT_1);
        vm.stopPrank();
    }

    function testLockRevertsInsufficientBalance() public {
        uint256 amount = 2000e18; // More than user has

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);

        vm.expectRevert();
        lockVault.lock(amount, AGENT_1);
        vm.stopPrank();
    }

    // ========== Vault Balance Tests ==========

    function testVaultBalanceInitiallyZero() public view {
        assertEq(lockVault.vaultBalance(), 0);
    }

    function testVaultBalanceAfterLock() public {
        uint256 amount = 100e18;

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);
        lockVault.lock(amount, AGENT_1);
        vm.stopPrank();

        assertEq(lockVault.vaultBalance(), amount);
    }

    // ========== Admin Withdraw Tests ==========

    function testAdminWithdraw() public {
        // First lock some tokens
        uint256 lockAmount = 100e18;
        vm.startPrank(user1);
        token.approve(address(lockVault), lockAmount);
        lockVault.lock(lockAmount, AGENT_1);
        vm.stopPrank();

        // Admin withdraws
        uint256 withdrawAmount = 50e18;
        address recipient = makeAddr("recipient");

        vm.expectEmit(true, true, true, true);
        emit AdminWithdraw(admin, withdrawAmount, recipient);

        vm.prank(admin);
        lockVault.adminWithdraw(withdrawAmount, recipient);

        assertEq(token.balanceOf(recipient), withdrawAmount);
        assertEq(lockVault.vaultBalance(), lockAmount - withdrawAmount);
    }

    function testAdminWithdrawRevertsNonAdmin() public {
        // First lock some tokens
        uint256 lockAmount = 100e18;
        vm.startPrank(user1);
        token.approve(address(lockVault), lockAmount);
        lockVault.lock(lockAmount, AGENT_1);
        vm.stopPrank();

        // Non-admin tries to withdraw
        vm.prank(user1);
        vm.expectRevert("HoloLockVault: only admin");
        lockVault.adminWithdraw(50e18, user1);
    }

    function testAdminWithdrawRevertsZeroRecipient() public {
        vm.prank(admin);
        vm.expectRevert("HoloLockVault: zero recipient");
        lockVault.adminWithdraw(50e18, address(0));
    }

    function testAdminWithdrawRevertsZeroAmount() public {
        vm.prank(admin);
        vm.expectRevert("HoloLockVault: zero amount");
        lockVault.adminWithdraw(0, admin);
    }

    // ========== Admin Recover Tokens Tests ==========

    function testAdminRecoverTokens() public {
        // Accidentally send tokens directly to contract
        MockHOT otherToken = new MockHOT();
        otherToken.mint(address(lockVault), 100e18);

        address recipient = makeAddr("recipient");

        vm.prank(admin);
        lockVault.adminRecoverTokens(address(otherToken), recipient);

        assertEq(otherToken.balanceOf(recipient), 100e18);
        assertEq(otherToken.balanceOf(address(lockVault)), 0);
    }

    function testAdminRecoverTokensRevertsNonAdmin() public {
        vm.prank(user1);
        vm.expectRevert("HoloLockVault: only admin");
        lockVault.adminRecoverTokens(address(token), user1);
    }

    function testAdminRecoverTokensRevertsZeroRecipient() public {
        vm.prank(admin);
        vm.expectRevert("HoloLockVault: zero recipient");
        lockVault.adminRecoverTokens(address(token), address(0));
    }

    function testAdminRecoverTokensRevertsNoBalance() public {
        vm.prank(admin);
        vm.expectRevert("HoloLockVault: no balance");
        lockVault.adminRecoverTokens(address(token), admin);
    }

    // ========== Set Admin Tests ==========

    function testSetAdmin() public {
        address newAdmin = makeAddr("newAdmin");

        vm.expectEmit(true, true, true, true);
        emit AdminChanged(admin, newAdmin);

        vm.prank(admin);
        lockVault.setAdmin(newAdmin);

        assertEq(lockVault.admin(), newAdmin);
    }

    function testSetAdminRevertsNonAdmin() public {
        vm.prank(user1);
        vm.expectRevert("HoloLockVault: only admin");
        lockVault.setAdmin(user1);
    }

    function testSetAdminRevertsZeroAddress() public {
        vm.prank(admin);
        vm.expectRevert("HoloLockVault: zero admin address");
        lockVault.setAdmin(address(0));
    }

    // ========== Set Min Lock Amount Tests ==========

    function testSetMinLockAmount() public {
        uint256 newMin = 5e18;

        vm.prank(admin);
        lockVault.setMinLockAmount(newMin);

        assertEq(lockVault.minLockAmount(), newMin);
    }

    function testSetMinLockAmountRevertsNonAdmin() public {
        vm.prank(user1);
        vm.expectRevert("HoloLockVault: only admin");
        lockVault.setMinLockAmount(5e18);
    }

    // ========== Fuzz Tests ==========

    function testFuzzLock(uint256 amount, bytes32 agent) public {
        vm.assume(amount >= MIN_LOCK_AMOUNT && amount <= 1000e18);
        vm.assume(agent != bytes32(0));

        vm.startPrank(user1);
        token.approve(address(lockVault), amount);
        uint256 lockId = lockVault.lock(amount, agent);
        vm.stopPrank();

        assertEq(lockId, 0);
        assertEq(lockVault.vaultBalance(), amount);
    }

    function testFuzzMultipleLocks(uint8 numLocks) public {
        vm.assume(numLocks > 0 && numLocks <= 10);

        uint256 amountPerLock = 10e18;
        uint256 totalAmount = uint256(numLocks) * amountPerLock;

        vm.startPrank(user1);
        token.approve(address(lockVault), totalAmount);

        for (uint8 i = 0; i < numLocks; i++) {
            bytes32 agent = keccak256(abi.encodePacked("agent", i));
            uint256 lockId = lockVault.lock(amountPerLock, agent);
            assertEq(lockId, i);
        }
        vm.stopPrank();

        assertEq(lockVault.lockNonce(), numLocks);
        assertEq(lockVault.vaultBalance(), totalAmount);
    }
}
