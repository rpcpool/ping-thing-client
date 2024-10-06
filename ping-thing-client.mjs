import {
  createTransactionMessage,
  pipe,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  createKeyPairFromBytes,
  createSignerFromKeyPair,
  appendTransactionMessageInstructions,
  signTransactionMessageWithSigners,
  SOLANA_ERROR__TRANSACTION_ERROR__BLOCKHASH_NOT_FOUND,
  isSolanaError,
  getSignatureFromTransaction,
  sendTransactionWithoutConfirmingFactory,
  // Address,
} from "@solana/web3.js";
import dotenv from "dotenv";
import bs58 from "bs58";
import { getSetComputeUnitLimitInstruction } from "@solana-program/compute-budget";
import { getTransferSolInstruction } from "@solana-program/system";
import { sleep } from "./utils/misc.mjs";
import { getLatestBlockhash } from "./utils/blockhash.mjs";
import { rpc, rpcSubscriptions } from "./utils/rpc.mjs";
import { getNextSlot } from "./utils/slot.mjs";
import { setMaxListeners } from "events";
import axios from "axios";
import { createRecentSignatureConfirmationPromiseFactory } from "@solana/transaction-confirmation";

dotenv.config();

function safeJSONStringify(val) {
  return JSON.stringify(val, (_, val) => typeof val === 'bigint' ? val.toString() : val);
}

const orignalConsoleLog = console.log;
console.log = function (...message) {
  const dateTime = new Date().toUTCString();
  orignalConsoleLog(dateTime, ...message);
};

// Catch interrupts & exit
process.on("SIGINT", function () {
  console.log(`Caught interrupt signal`, "\n");
  process.exit();
});

const SLEEP_MS_RPC = process.env.SLEEP_MS_RPC || 2000;
const SLEEP_MS_LOOP = process.env.SLEEP_MS_LOOP || 0;
const VA_API_KEY = process.env.VA_API_KEY;
const VERBOSE_LOG = process.env.VERBOSE_LOG === "true" ? true : false;
const COMMITMENT_LEVEL = process.env.COMMITMENT || "confirmed";
const USE_PRIORITY_FEE = process.env.USE_PRIORITY_FEE == "true" ? true : false;
const SKIP_VALIDATORS_APP = process.env.SKIP_VALIDATORS_APP || false;

if (VERBOSE_LOG) console.log(`Starting script`);

let USER_KEYPAIR;
const TX_RETRY_INTERVAL = 2000;

setMaxListeners(100);

const mConfirmRecentSignature = createRecentSignatureConfirmationPromiseFactory({
  rpc,
  rpcSubscriptions,
});
const mSendTransactionWithoutConfirming = sendTransactionWithoutConfirmingFactory({ rpc });

async function pingThing() {
  USER_KEYPAIR = await createKeyPairFromBytes(
    bs58.decode(process.env.WALLET_PRIVATE_KEYPAIR)
  );
  // Pre-define loop constants & variables
  const FAKE_SIGNATURE =
    "9999999999999999999999999999999999999999999999999999999999999999999999999999999999999999";

  // Run inside a loop that will exit after 3 consecutive failures
  const MAX_TRIES = 3;
  let tryCount = 0;

  const signer = await createSignerFromKeyPair(USER_KEYPAIR);
  const BASE_TRANSACTION_MESSAGE = pipe(
    createTransactionMessage({ version: 0 }),
    (tx) => setTransactionMessageFeePayerSigner(signer, tx),
    (tx) =>
      appendTransactionMessageInstructions(
        [
          getSetComputeUnitLimitInstruction({
            units: 500,
          }),
          getTransferSolInstruction({
            source: signer,
            destination: signer.address,
            amount: 5000,
          }),
        ],
        tx
      )
  );
  while (true) {
    await sleep(SLEEP_MS_LOOP);

    let slotSent;
    let slotLanded;
    let signature;
    let txStart;
    let txSendAttempts = 1;

    try {
      const pingAbortController = new AbortController();
      try {
        const latestBlockhash = await getLatestBlockhash();
        const transactionMessage = setTransactionMessageLifetimeUsingBlockhash(
          latestBlockhash,
          BASE_TRANSACTION_MESSAGE
        );
        const transactionSignedWithFeePayer =
          await signTransactionMessageWithSigners(transactionMessage);
        signature = getSignatureFromTransaction(transactionSignedWithFeePayer);

        console.log(`Sending ${signature}`);

        let sendAbortController;
        function sendTransaction() {
          sendAbortController = new AbortController()
          mSendTransactionWithoutConfirming(transactionSignedWithFeePayer, {
            abortSignal: sendAbortController.signal,
            commitment: COMMITMENT_LEVEL,
            maxRetries: 0n,
            skipPreflight: true,
          }).catch(e => {
            if (e instanceof Error && e.name === 'AbortError') {
              return;
            } else {
              throw e;
            }
          });
        }
        slotSent = await getNextSlot();
        const sendRetryInterval = setInterval(() => {
          sendAbortController.abort();
          console.log(`Tx not confirmed after ${TX_RETRY_INTERVAL * txSendAttempts++}ms, resending`);
          sendTransaction();
        }, TX_RETRY_INTERVAL);
        pingAbortController.signal.addEventListener('abort', () => {
          clearInterval(sendRetryInterval);
          sendAbortController.abort();
        });
        txStart = Date.now();
        sendTransaction();
        await mConfirmRecentSignature({
          abortSignal: pingAbortController.signal,
          commitment: COMMITMENT_LEVEL,
          signature,
        });
        console.log(`Confirmed tx ${signature}`);
      } catch (e) {
        // Log and loop if we get a bad blockhash.
        if (isSolanaError(e, SOLANA_ERROR__TRANSACTION_ERROR__BLOCKHASH_NOT_FOUND)) {
        // if (e.message.includes("Blockhash not found")) {
          console.log(`ERROR: Blockhash not found`);
          continue;
        }

        // If the transaction expired on the chain. Make a log entry and send
        // to VA. Otherwise log and loop.
        if (e.name === "TransactionExpiredBlockheightExceededError") {
          console.log(
            `ERROR: Blockhash expired/block height exceeded. TX failure sent to VA.`
          );
        } else {
          console.log(`ERROR: ${e.name}`);
          console.log(e.message);
          console.log(e);
          console.log(safeJSONStringify(e));
          continue;
        }

        // Need to submit a fake signature to pass the import filters
        signature = FAKE_SIGNATURE;
      } finally {
        pingAbortController.abort();
      }

      const txEnd = Date.now();
      // Sleep a little here to ensure the signature is on an RPC node.
      await sleep(SLEEP_MS_RPC);
      if (signature !== FAKE_SIGNATURE) {
        // Capture the slotLanded
        let txLanded = await rpc
          .getTransaction(signature, {
            commitment: COMMITMENT_LEVEL,
            maxSupportedTransactionVersion: 255,
          })
          .send();
        if (txLanded === null) {
          console.log(
            signature,
            `ERROR: tx is not found on RPC within ${SLEEP_MS_RPC}ms. Not sending to VA.`
          );
          continue;
        }
        slotLanded = txLanded.slot;
      }

      // Don't send if the slot latency is negative
      if (slotLanded < slotSent) {
        console.log(
          signature,
          `ERROR: Slot ${slotLanded} < ${slotSent}. Not sending to VA.`
        );
        continue;
      }

      // prepare the payload to send to validators.app
      const vAPayload = safeJSONStringify({
        time: txEnd - txStart,
        signature,
        transaction_type: "transfer",
        success: signature !== FAKE_SIGNATURE,
        application: "web3",
        commitment_level: COMMITMENT_LEVEL,
        slot_sent: BigInt(slotSent).toString(),
        slot_landed: BigInt(slotLanded).toString(),
      });
      if (VERBOSE_LOG) {
        console.log(vAPayload);
      }

      if (!SKIP_VALIDATORS_APP) {
        // Send the payload to validators.app
        const vaResponse = await axios.post(
          "https://www.validators.app/api/v1/ping-thing/mainnet",
          vAPayload,
          {
            headers: {
              "Content-Type": "application/json",
              Token: VA_API_KEY,
            },
          }
        );
        // throw error if response is not ok
        if (!(vaResponse.status >= 200 && vaResponse.status <= 299)) {
          throw new Error(`Failed to update validators: ${vaResponse.status}`);
        }

        if (VERBOSE_LOG) {
          console.log(
            `VA Response ${vaResponse.status
            } ${safeJSONStringify(vaResponse.data)}`
          );
        }
      }

      // Reset the try counter
      tryCount = 0;
    } catch (e) {
      console.log(`ERRORR: ${e}`);
      console.log(e);
      if (++tryCount === MAX_TRIES) throw e;
    }
  }
}

await pingThing();
