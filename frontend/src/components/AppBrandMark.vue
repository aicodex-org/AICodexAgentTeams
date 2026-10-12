<script setup lang="ts">
import { useId } from 'vue'

const id = useId().replaceAll(':', '')
const haloId = `${id}-brand-halo`
const diskId = `${id}-brand-disk`
const glowId = `${id}-brand-glow`
</script>

<template>
  <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" aria-hidden="true" focusable="false">
    <defs>
      <linearGradient :id="haloId" x1="0" y1="0" x2="0.8" y2="1">
        <stop offset="0" stop-color="#fff3d6" />
        <stop offset="0.5" stop-color="#ffc981" />
        <stop offset="1" stop-color="#f2a653" />
      </linearGradient>
      <linearGradient :id="diskId">
        <stop offset="0" stop-color="#f2a653" stop-opacity="0" />
        <stop offset="0.24" stop-color="#ffc981" />
        <stop offset="0.5" stop-color="#fff8e8" />
        <stop offset="0.76" stop-color="#ffc981" />
        <stop offset="1" stop-color="#f2a653" stop-opacity="0" />
      </linearGradient>
      <filter :id="glowId" x="-50%" y="-50%" width="200%" height="200%">
        <feGaussianBlur stdDeviation="2" />
      </filter>
    </defs>

    <rect width="64" height="64" rx="14" fill="#0b111b" />
    <g fill="none" :stroke="`url(#${haloId})`">
      <circle cx="32" cy="32" r="18" stroke-width="5" opacity="0.45" :filter="`url(#${glowId})`" />
      <circle cx="32" cy="32" r="18" stroke-width="3" />
      <circle cx="32" cy="32" r="14.5" stroke-width="0.8" opacity="0.65" />
    </g>
    <circle cx="32" cy="32" r="13.5" fill="#0b111b" />
    <path
      d="M3 34C15 31 49 31 61 30"
      fill="none"
      :stroke="`url(#${diskId})`"
      stroke-width="5"
      :filter="`url(#${glowId})`"
      opacity="0.6"
    />
    <path
      d="M3 34C15 31 49 31 61 30"
      fill="none"
      :stroke="`url(#${diskId})`"
      stroke-width="1.8"
      stroke-linecap="round"
    />
  </svg>
</template>
