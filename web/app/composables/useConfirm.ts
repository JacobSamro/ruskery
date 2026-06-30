// Promise-based confirmation backed by a single shadcn AlertDialog mounted in
// app.vue. Replaces window.confirm: `if (!(await confirm({...}))) return;`.
import { reactive } from "vue";

export interface ConfirmOptions {
  title?: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  destructive?: boolean;
}

const state = reactive({
  open: false,
  title: "Are you sure?",
  message: "",
  confirmText: "Confirm",
  cancelText: "Cancel",
  destructive: false,
});

let resolver: ((value: boolean) => void) | null = null;

export function useConfirm() {
  function confirm(opts: ConfirmOptions): Promise<boolean> {
    state.title = opts.title ?? "Are you sure?";
    state.message = opts.message;
    state.confirmText = opts.confirmText ?? "Confirm";
    state.cancelText = opts.cancelText ?? "Cancel";
    state.destructive = opts.destructive ?? false;
    state.open = true;
    return new Promise((resolve) => {
      resolver = resolve;
    });
  }

  // Explicit choice from the Cancel/Action buttons — resolves immediately so it
  // never races the dialog's own close emit.
  function decide(value: boolean) {
    state.open = false;
    if (resolver) {
      resolver(value);
      resolver = null;
    }
  }

  // The dialog also emits close on Esc/overlay AND when an action/cancel button
  // is pressed. Defer the implicit "false" to a microtask so an explicit
  // decide(true/false) from a button click (same tick) always wins the race.
  function onOpenChange(open: boolean) {
    state.open = open;
    if (!open) {
      queueMicrotask(() => {
        if (resolver) {
          resolver(false);
          resolver = null;
        }
      });
    }
  }

  return { state, confirm, decide, onOpenChange };
}
