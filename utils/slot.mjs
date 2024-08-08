import { createCipheriv } from "crypto";
import { rpcSubscriptions } from "./rpc.mjs";

const MAX_SLOT_FETCH_ATTEMPTS = process.env.MAX_SLOT_FETCH_ATTEMPTS || 100;

let resolveSlotPromise;
let nextSlotPromise;

(async () => {
  let attempts = 0;
  while (true) {
    try {
      const slotNotifications = await rpcSubscriptions
        .slotsUpdatesNotifications()
        .subscribe({ abortSignal: AbortSignal.any([]) });
      for await (const { slot, type } of slotNotifications) {
        if (type === "firstShredReceived") {
          attempts = 0;
          if (resolveSlotPromise) {
            resolveSlotPromise(slot);
          }
          resolveSlotPromise = undefined;
          nextSlotPromise = undefined;
          continue;
        }
        if (++attempts >= MAX_SLOT_FETCH_ATTEMPTS) {
          console.log(
            `ERROR: Max attempts for fetching slot type "firstShredReceived" reached, exiting`
          );
          process.exit(0);
        }
      }
    } catch (e) {
      console.log(`ERROR: ${e}`);
      ++attempts;
    }
  }
})();

export async function getNextSlot() {
  nextSlotPromise ||= new Promise((resolve) => {
    resolveSlotPromise = resolve
  });
  return await nextSlotPromise;
}
