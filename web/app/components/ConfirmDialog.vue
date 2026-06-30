<script setup lang="ts">
// Global confirmation dialog (one instance, mounted in app.vue). Driven by the
// useConfirm() composable so any view can `await confirm({...})`.
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

const { state, decide, onOpenChange } = useConfirm();
</script>

<template>
  <AlertDialog :open="state.open" @update:open="onOpenChange">
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogTitle>{{ state.title }}</AlertDialogTitle>
        <AlertDialogDescription>{{ state.message }}</AlertDialogDescription>
      </AlertDialogHeader>
      <AlertDialogFooter>
        <AlertDialogCancel @click="decide(false)">{{ state.cancelText }}</AlertDialogCancel>
        <AlertDialogAction
          :class="state.destructive ? 'bg-red-600 text-white hover:bg-red-600/90 focus-visible:ring-red-600/40' : ''"
          @click="decide(true)"
        >
          {{ state.confirmText }}
        </AlertDialogAction>
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>
</template>
