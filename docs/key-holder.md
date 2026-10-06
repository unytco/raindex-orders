# Holding a multisig key

You are asked to hold one key of a Safe multisig for the HOT bridge. To act, the Safe needs the approval of two of its three key holders. Your key alone cannot move funds, and losing your key alone loses nothing.

## What you need

One of these:

- **A Ledger hardware wallet.** This is the better choice.
- **A Foundry encrypted keystore** on a computer only you use. Install Foundry from <https://getfoundry.sh>.

Never keep the key in a plain file, a note, a password manager entry or a chat message.

## Create your key

With a Ledger:

1. Set up the Ledger and install its Ethereum app.
2. Write the 24-word recovery phrase on paper. Keep the paper offline, away from the Ledger.
3. Connect the Ledger, open the Ethereum app, and run:

   ```sh
   cast wallet address --ledger
   ```

With a keystore:

1. Create the key. When it asks for a password, choose a strong one:

   ```sh
   cast wallet new ~/.foundry/keystores bridge-safe-key
   ```

   To use a key you already have, run `cast wallet import bridge-safe-key --interactive` instead. It asks for the key, so the key never appears in a command.
2. Print its address:

   ```sh
   cast wallet address --account bridge-safe-key
   ```

3. Back up the file `~/.foundry/keystores/bridge-safe-key`, and keep the password apart from it. The file is useless without the password.

## Send only the address

Send the operator the address that `cast wallet address` printed. Never send the recovery phrase, the keystore file or the password. Nobody from the project will ask for them.

## Approve a Safe transaction

The operator tells you, through a second channel, what each transaction does: which contract it calls and what it changes. Approve only a transaction you were told to expect.

In the Safe web app, which works with a Ledger:

1. Open <https://app.safe.global> and connect your Ledger or wallet.
2. Open the Safe the operator names, then its pending transaction.
3. Make sure that the contract it calls, and what it changes, match what the operator told you.
4. Select **Confirm** and sign on your device.

With `cast` and a keystore:

1. The operator sends you the Safe's transaction hash. Open the transaction in the Safe web app, make sure it is the one you expect, and make sure the app shows the same hash.
2. Sign it:

   ```sh
   cast wallet sign --no-hash <transaction hash> --account bridge-safe-key
   ```

3. Send the signature to the operator. A signature is not secret: it approves only that one transaction.

## Keep it safe

- Keep the Ledger and its recovery phrase in different places.
- Use the key only to approve transactions the operator told you to expect.
- If anything about a request looks wrong, do not sign. Ask the operator through a second channel.

## If your key is lost or exposed

Tell the operator at once, through a second channel.

- **Lost:** restore it from the recovery phrase or the keystore backup. If you cannot, the other key holders replace your key in the Safe.
- **Exposed**, or you think someone else has a copy: even if you still have it, the other key holders replace your key in the Safe. Do not use it again.

Replacing a key takes two approvals, so the Safe stays safe while it happens.
