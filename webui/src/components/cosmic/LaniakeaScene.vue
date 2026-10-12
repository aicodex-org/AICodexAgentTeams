<script setup lang="ts">
import { usePreferredReducedMotion } from '@vueuse/core'
import { BufferGeometry, Color, Float32BufferAttribute, Group, LineBasicMaterial, LineSegments, PerspectiveCamera, Points, PointsMaterial, Scene, Vector3, WebGLRenderer } from 'three'
import { OrbitControls } from 'three/addons/controls/OrbitControls.js'
import { onBeforeUnmount, onMounted, shallowRef, useTemplateRef, watch } from 'vue'
import { useThemeStore } from '@/stores/modules/theme'

const props = defineProps<{ playing: boolean, decorative?: boolean }>()
const emit = defineEmits<{ ready: [count: number], retry: [], failed: [] }>()
const host = useTemplateRef<HTMLDivElement>('host')
const markerElements = useTemplateRef<HTMLDivElement[]>('markerElements')
// 银河系在当前尺度下近似本星系群原点，吸引中心采用同一 CF4 非分组重建
// 坐标来源：https://arxiv.org/html/2305.02339v2#S3，单位 Mpc/h
const landmarks = [
  { id: 'milky-way', label: '银河系', detail: '我们的位置', position: new Vector3(0, 0, 0) },
  { id: 'great-attractor', label: '巨引源', detail: 'CF4 吸引中心', position: new Vector3(-62, -8, 39) },
]
const projected = new Vector3()
const error = shallowRef('')
const theme = useThemeStore()
const reducedMotion = usePreferredReducedMotion()
const abort = new AbortController()
let renderer: WebGLRenderer | undefined
let controls: OrbitControls | undefined
let observer: ResizeObserver | undefined
let camera: PerspectiveCamera | undefined
let scene: Scene | undefined
let resetView: (() => void) | undefined
const disposables: Array<{ dispose: () => void }> = []
let disposed = false
let elapsed = 0
let previousTime = 0
let needsRender = true
let lineMaterial: LineBasicMaterial | undefined
let pointMaterial: PointsMaterial | undefined

function applyTheme() {
  needsRender = true
  const dark = theme.effectiveTheme === 'dark'
  lineMaterial?.color.set(dark ? '#e9b66e' : '#9d632c')
  if (lineMaterial)
    lineMaterial.opacity = dark ? 0.33 : 0.27
  pointMaterial?.color.set(dark ? '#fff0c7' : '#ad6a21')
}

function visibilityChanged() {
  previousTime = 0
}

function contextLost(event: Event) {
  event.preventDefault()
  renderer?.setAnimationLoop(null)
  error.value = '三维画面已暂停，请重新加载'
  emit('failed')
}

function reset() {
  resetView?.()
}

function updateMarkers() {
  if (props.decorative || !host.value || !camera)
    return
  const { clientWidth: width, clientHeight: height } = host.value
  for (const element of markerElements.value ?? []) {
    const landmark = landmarks.find(item => item.id === element.dataset.landmark)
    if (!landmark)
      continue
    projected.copy(landmark.position).project(camera)
    const visible = projected.z >= -1 && projected.z <= 1 && Math.abs(projected.x) < 1 && Math.abs(projected.y) < 1
    element.hidden = !visible
    if (visible) {
      const x = (projected.x + 1) * width / 2
      const y = (1 - projected.y) * height / 2
      element.style.transform = `translate(${x}px, ${y}px)`
      element.classList.toggle('label-left', x > width - 150)
      element.classList.toggle('label-below', y < 65 || (landmark.id === 'milky-way' && y < height - 65))
    }
  }
}

defineExpose({ reset })

onMounted(async () => {
  try {
    const response = await fetch('/data/laniakea.json', { signal: abort.signal })
    if (!response.ok)
      throw new Error('数据加载失败')
    const data: { lines: number[][] } = await response.json()
    if (disposed || !host.value)
      return
    if (!Array.isArray(data.lines) || !data.lines.length || data.lines.some(line => line.length < 6 || line.length % 3 || line.some(value => !Number.isFinite(value))))
      throw new Error('数据格式无效')

    renderer = new WebGLRenderer({ antialias: true, alpha: true, powerPreference: 'low-power' })
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 1.75))
    if (!props.decorative) {
      renderer.domElement.setAttribute('aria-label', '拉尼亚凯亚三维速度场，拖动旋转，滚轮缩放，Shift 加方向键旋转')
      renderer.domElement.setAttribute('tabindex', '0')
    }
    renderer.domElement.addEventListener('webglcontextlost', contextLost)
    host.value.appendChild(renderer.domElement)
    scene = new Scene()
    camera = new PerspectiveCamera(38, 1, 1, 2000)
    const group = new Group()
    scene.add(group)

    const vertices: number[] = []
    const colors: number[] = []
    const color = new Color()
    for (const line of data.lines) {
      for (let i = 0; i < line.length - 3; i += 3) {
        vertices.push(...line.slice(i, i + 3), ...line.slice(i + 3, i + 6))
        // 只按流线进度调节亮度，不将装饰色解释为速度测量
        color.setScalar(0.45 + 0.55 * i / line.length)
        colors.push(color.r, color.g, color.b, color.r, color.g, color.b)
      }
    }
    const geometry = new BufferGeometry()
    geometry.setAttribute('position', new Float32BufferAttribute(vertices, 3))
    geometry.setAttribute('color', new Float32BufferAttribute(colors, 3))
    geometry.computeBoundingSphere()
    const center = geometry.boundingSphere!.center
    const radius = geometry.boundingSphere!.radius
    disposables.push(geometry)
    if (!Number.isFinite(radius) || radius <= 0)
      throw new Error('流线范围无效')
    lineMaterial = new LineBasicMaterial({ transparent: true, depthWrite: false, vertexColors: true })
    group.add(new LineSegments(geometry, lineMaterial))
    disposables.push(lineMaterial)

    const particleLines = data.lines.filter((_, i) => i % 3 === 0)
    const particles = new Float32Array(particleLines.length * 3)
    const particleGeometry = new BufferGeometry()
    particleGeometry.setAttribute('position', new Float32BufferAttribute(particles, 3))
    pointMaterial = new PointsMaterial({ size: 1.1, transparent: true, opacity: 0.8, depthWrite: false, sizeAttenuation: true })
    const points = new Points(particleGeometry, pointMaterial)
    points.frustumCulled = false
    group.add(points)
    disposables.push(particleGeometry, pointMaterial)

    // 原点为本星系群位置，保留真实坐标，不将标记放到视觉中心
    const originGeometry = new BufferGeometry().setFromPoints([new Vector3(0, 0, 0)])
    const originMaterial = new PointsMaterial({ color: '#7d9aaf', size: 3, sizeAttenuation: true })
    group.add(new Points(originGeometry, originMaterial))
    disposables.push(originGeometry, originMaterial)
    controls = new OrbitControls(camera, renderer.domElement)
    controls.enabled = !props.decorative
    controls.enableDamping = true
    controls.enablePan = false
    controls.minDistance = radius * 1.2
    controls.maxDistance = radius * 7
    controls.autoRotateSpeed = 0.3
    if (!props.decorative)
      controls.listenToKeyEvents(renderer.domElement)
    controls.target.copy(center)
    resetView = () => {
      if (!camera || !controls || !host.value)
        return
      const aspect = Math.max(host.value.clientWidth, 1) / Math.max(host.value.clientHeight, 1)
      // 首屏以近景展示流线，允许外围枝条出画，避免包围球留出过多空白
      const framing = aspect < 1 ? 0.68 : 0.55
      const distance = radius / Math.sin(19 * Math.PI / 180) * Math.max(1, 1 / aspect) * framing
      camera.position.copy(center).add(new Vector3(0.25, -0.55, 1).normalize().multiplyScalar(distance))
      camera.up.set(0, 1, 0)
      controls.target.copy(center)
      controls.update()
      needsRender = true
    }
    const resize = () => {
      if (!host.value || !camera || !renderer)
        return
      const { clientWidth: width, clientHeight: height } = host.value
      camera.aspect = width / Math.max(height, 1)
      camera.updateProjectionMatrix()
      renderer.setSize(width, height)
      resetView?.()
      needsRender = true
    }
    observer = new ResizeObserver(resize)
    observer.observe(host.value)
    resize()
    resetView()
    applyTheme()
    document.addEventListener('visibilitychange', visibilityChanged)
    renderer.setAnimationLoop((time) => {
      if (document.hidden || !renderer || !scene || !camera || !controls)
        return
      const delta = previousTime ? Math.min((time - previousTime) / 1000, 0.05) : 0
      previousTime = time
      const animate = props.playing && reducedMotion.value !== 'reduce'
      if (animate)
        elapsed += delta
      controls.autoRotate = animate
      const changed = controls.update(delta)
      if (!animate && !changed && !needsRender)
        return
      needsRender = false
      const attribute = particleGeometry.getAttribute('position')
      particleLines.forEach((line, i) => {
        const count = line.length / 3
        const progress = ((elapsed * 0.045 + i * 0.618033) % 1) * (count - 1)
        const index = Math.floor(progress) * 3
        const fraction = progress % 1
        attribute.setXYZ(i, line[index]! + (line[index + 3]! - line[index]!) * fraction, line[index + 1]! + (line[index + 4]! - line[index + 1]!) * fraction, line[index + 2]! + (line[index + 5]! - line[index + 2]!) * fraction)
      })
      attribute.needsUpdate = true
      renderer.render(scene, camera)
      updateMarkers()
    })
    emit('ready', data.lines.length)
  }
  catch {
    if (!disposed) {
      releaseGraphics()
      error.value = '无法加载三维星图，请检查网络或浏览器 WebGL 支持'
      emit('failed')
    }
  }
})

function releaseGraphics() {
  observer?.disconnect()
  document.removeEventListener('visibilitychange', visibilityChanged)
  renderer?.setAnimationLoop(null)
  renderer?.domElement.removeEventListener('webglcontextlost', contextLost)
  controls?.dispose()
  disposables.splice(0).forEach(resource => resource.dispose())
  renderer?.dispose()
  renderer?.forceContextLoss()
  renderer?.domElement.remove()
  renderer = undefined
  controls = undefined
  camera = undefined
  scene = undefined
  resetView = undefined
}

watch(() => theme.effectiveTheme, applyTheme)

onBeforeUnmount(() => {
  disposed = true
  abort.abort()
  releaseGraphics()
})
</script>

<template>
  <div ref="host" class="cosmic-canvas">
    <div v-if="!decorative && !error" class="scene-markers">
      <div
        v-for="landmark in landmarks"
        :key="landmark.id"
        ref="markerElements"
        :data-landmark="landmark.id"
        class="scene-marker"
        :class="landmark.id"
        hidden
      >
        <span class="marker-dot" />
        <span class="marker-label"><strong>{{ landmark.label }}</strong><small>{{ landmark.detail }}</small></span>
      </div>
    </div>
    <div v-if="error && !decorative" class="scene-error" role="status">
      <p>{{ error }}</p>
      <button type="button" @click="$emit('retry')">
        重新加载
      </button>
    </div>
  </div>
</template>

<style scoped>
.cosmic-canvas {
  position: relative;
  width: 100%;
  height: 100%;
}
.scene-markers {
  position: absolute;
  inset: 0;
  overflow: hidden;
  pointer-events: none;
}
.scene-marker {
  position: absolute;
  top: 0;
  left: 0;
  color: var(--cosmic-local);
}
.great-attractor {
  color: var(--cosmic-accent);
}
.marker-dot {
  position: absolute;
  width: 8px;
  height: 8px;
  transform: translate(-50%, -50%);
  border: 2px solid var(--cp-color-bg-container);
  border-radius: 50%;
  background: currentColor;
  box-shadow:
    0 0 0 1px currentColor,
    0 0 0 6px color-mix(in srgb, currentColor 14%, transparent);
}
.marker-label {
  position: absolute;
  bottom: 16px;
  left: 16px;
  padding: 5px 9px;
  border-left: 1px solid currentColor;
  border-radius: 0 4px 4px 0;
  background: color-mix(in srgb, var(--cp-color-bg-layout) 90%, transparent);
  white-space: nowrap;
}
.marker-label::before {
  position: absolute;
  bottom: -11px;
  left: -15px;
  width: 19px;
  border-top: 1px solid currentColor;
  transform: rotate(-45deg);
  content: '';
  opacity: 0.65;
}
.marker-label strong {
  display: block;
  font-size: 13px;
  font-weight: 600;
  letter-spacing: 1px;
}
.marker-label small {
  display: block;
  margin-top: 2px;
  color: var(--cp-color-text-secondary);
  font-size: 10px;
}
.label-left .marker-label {
  right: 16px;
  left: auto;
}
.label-left .marker-label::before {
  right: -15px;
  left: auto;
  transform: rotate(45deg);
}
.label-below .marker-label {
  top: 16px;
  bottom: auto;
}
.label-below .marker-label::before {
  top: -11px;
  bottom: auto;
  transform: rotate(45deg);
}
.label-left.label-below .marker-label::before {
  transform: rotate(-45deg);
}
.cosmic-canvas :deep(canvas) {
  display: block;
  touch-action: none;
  outline-offset: -4px;
}
.scene-error {
  position: absolute;
  inset: 40% 20px auto;
  text-align: center;
  color: var(--cp-color-text-secondary);
}
.scene-error button {
  margin-top: 12px;
  text-decoration: underline;
  cursor: pointer;
}
</style>
