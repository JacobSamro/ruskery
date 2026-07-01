<script setup lang="ts">
import { Sun, Moon, Monitor } from "@lucide/vue";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// Provided by @nuxtjs/color-mode. `preference` is light|dark|system (persisted);
// `value` is the resolved light|dark used to render the trigger icon.
const colorMode = useColorMode();
</script>

<template>
  <DropdownMenu>
    <DropdownMenuTrigger as-child>
      <button
        type="button"
        aria-label="Toggle theme"
        class="inline-flex h-9 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
      >
        <Sun
          aria-hidden="true"
          class="h-[1.15rem] w-[1.15rem] rotate-0 scale-100 transition-all dark:-rotate-90 dark:scale-0"
        />
        <Moon
          aria-hidden="true"
          class="absolute h-[1.15rem] w-[1.15rem] rotate-90 scale-0 transition-all dark:rotate-0 dark:scale-100"
        />
        <span class="sr-only">Toggle theme</span>
      </button>
    </DropdownMenuTrigger>
    <DropdownMenuContent align="end" class="min-w-32">
      <DropdownMenuItem
        :data-active="colorMode.preference === 'light'"
        @click="colorMode.preference = 'light'"
      >
        <Sun aria-hidden="true" class="size-4" /> Light
      </DropdownMenuItem>
      <DropdownMenuItem
        :data-active="colorMode.preference === 'dark'"
        @click="colorMode.preference = 'dark'"
      >
        <Moon aria-hidden="true" class="size-4" /> Dark
      </DropdownMenuItem>
      <DropdownMenuItem
        :data-active="colorMode.preference === 'system'"
        @click="colorMode.preference = 'system'"
      >
        <Monitor aria-hidden="true" class="size-4" /> System
      </DropdownMenuItem>
    </DropdownMenuContent>
  </DropdownMenu>
</template>
