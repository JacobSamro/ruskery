<script setup lang="ts">
const props = withDefaults(
  defineProps<{ variant?: "default" | "outline" | "primary" }>(),
  { variant: "default" },
);
const variants: Record<string, string> = {
  default: "bg-[var(--color-bg)] text-[var(--color-muted)] border-[var(--color-border)]",
  outline: "border-[var(--color-border)] text-[var(--color-fg)]",
  primary: "bg-[var(--color-primary)] text-[var(--color-primary-fg)] border-transparent",
};
</script>

<template>
  <span
    :class="[
      'inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium',
      variants[props.variant],
    ]"
  >
    <slot />
  </span>
</template>
