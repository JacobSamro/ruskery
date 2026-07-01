<script setup lang="ts">
// `UiButton` — the project's button, backed by the official shadcn-vue Button.
// Lives at the components root (not ui/) so the wrapper filename can't collide
// with the ui/button/ component dir on a case-insensitive filesystem.
import { Button } from "@/components/ui/button";

withDefaults(
  defineProps<{
    variant?: "default" | "outline" | "ghost" | "destructive" | "secondary" | "link";
    size?: "default" | "sm" | "lg" | "icon";
    type?: "button" | "submit";
    disabled?: boolean;
  }>(),
  { variant: "default", size: "default", type: "button", disabled: false },
);
</script>

<template>
  <Button :variant="variant" :size="size" :type="type" :disabled="disabled">
    <slot />
  </Button>
</template>
