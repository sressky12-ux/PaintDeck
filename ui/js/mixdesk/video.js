// 影片預覽：<video> 跟著 Engine 的播放時間走（offset 之前、片尾之後就停在那裡）。
// 原聲接進 Engine 當一軌（attachMedia），所以音量、靜音、獨奏跟其他軌一樣；偏差小就微調播放速度，偏差大才跳（一直跳會卡）。
export class VideoSync {
  constructor(el, engine, box) {
    this.el = el;
    this.engine = engine;
    this.box = box; // 外框（沒有影片時加 .none）
    this.url = null;
    this.offset = 0;
    this.attached = false;
    this.onDuration = null;
    el.addEventListener("loadedmetadata", () => { if (this.onDuration) this.onDuration(el.duration || 0); });
  }

  /** url＝null 表示這首沒有影片 */
  set(url, offsetSec) {
    this.offset = offsetSec || 0;
    this.box.classList.toggle("none", !url);
    if (url === this.url) return;
    this.url = url;
    if (!url) {
      this.el.pause();
      this.el.removeAttribute("src");
      this.el.load();
      return;
    }
    if (!this.attached) {
      this.engine.attachMedia("video", this.el); // 之後聲音只從 Web Audio 出來
      this.el.muted = false;
      this.attached = true;
    }
    this.el.src = url;
  }

  get duration() { return this.url && Number.isFinite(this.el.duration) ? this.el.duration : 0; }

  tick(sec, playing) {
    if (!this.url || this.el.readyState < 1) return;
    const el = this.el;
    const t = sec - this.offset;
    const dur = this.duration;
    if (t < 0 || (dur && t >= dur)) {
      if (!el.paused) el.pause();
      const edge = t < 0 ? 0 : Math.max(0, dur - 0.05);
      if (Math.abs(el.currentTime - edge) > 0.04) el.currentTime = edge;
      return;
    }
    if (!playing) {
      if (!el.paused) el.pause();
      if (Math.abs(el.currentTime - t) > 0.04) el.currentTime = t;
      return;
    }
    if (el.paused) {
      el.currentTime = t;
      el.play().catch(() => { /* 還沒按過任何鍵時瀏覽器不准播；下次再試 */ });
      return;
    }
    const drift = el.currentTime - t;
    if (Math.abs(drift) > 0.25) { el.currentTime = t; el.playbackRate = 1; }
    else el.playbackRate = Math.abs(drift) > 0.04 ? (drift > 0 ? 0.95 : 1.05) : 1;
  }
}
