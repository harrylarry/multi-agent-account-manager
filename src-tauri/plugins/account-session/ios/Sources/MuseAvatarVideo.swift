// iOS-only presentation guard for Muse avatar videos.
enum MuseAvatarVideo {
  static let paintScript = #"""
  (() => {
    const host = location.hostname;
    if (host !== 'muse.ai' && !host.endsWith('.muse.ai')) return;
    const avatars = '[data-hatch-avatar-host] video, video[data-hatch-media-owner="avatar-layer"], [data-hatch-media-owner="avatar-layer"] video';
    const pendingClass = 'maam-avatar-awaiting-frame';
    const tracked = new WeakMap();

    const style = document.createElement('style');
    style.textContent = `
      [data-hatch-avatar-host]:has(video), [data-hatch-media-owner="avatar-layer"]:has(video) { overflow: clip; }
      ${avatars} { max-width: 100%; max-height: 100%; }
      video.${pendingClass}[data-hatch-media-owner="avatar-layer"],
      [data-hatch-avatar-host] video.${pendingClass}, [data-hatch-media-owner="avatar-layer"] video.${pendingClass} { opacity: 0 !important; }
    `;
    const installStyle = () => {
      if (document.documentElement && !style.isConnected) document.documentElement.prepend(style);
    };

    function show(video, state) {
      if (tracked.get(video) !== state) return;
      clearTimeout(state.fallback);
      if (state.frame !== undefined) video.cancelVideoFrameCallback?.(state.frame);
      state.pending = false;
      video.classList.remove(pendingClass);
    }

    function awaitFrame(video, state) {
      if (!state?.pending || state.fallback !== undefined || video.readyState < 2) return;
      if (typeof video.requestVideoFrameCallback === 'function') {
        state.frame = video.requestVideoFrameCallback(() => show(video, state));
        // A background or offscreen player may not deliver the callback.
        state.fallback = setTimeout(() => {
          state.fallback = undefined;
          if (video.readyState >= 2) show(video, state);
          else if (state.frame !== undefined) {
            video.cancelVideoFrameCallback?.(state.frame);
            state.frame = undefined;
          }
        }, 250);
      } else {
        state.fallback = setTimeout(() => {
          state.fallback = undefined;
          if (video.readyState >= 2) show(video, state);
        }, 34);
      }
    }

    function watch(video, newSource = false) {
      const previous = tracked.get(video);
      if (!video.matches(avatars) || !video.autoplay) {
        if (previous) {
          show(video, previous);
          tracked.delete(video);
        }
        return;
      }
      // The WKWebView setting also requires the actual video element
      // to opt in. Scope this to autoplay avatars; message videos keep
      // their original inline/fullscreen behavior.
      if (!video.playsInline) video.playsInline = true;
      if (previous && !newSource) return;
      if (previous) show(video, previous);
      const pending = newSource || (video.readyState < 2 && video.currentTime === 0);
      const state = { pending };
      tracked.set(video, state);
      video.classList.toggle(pendingClass, pending);
    }

    function scan(node) {
      if (node.nodeType !== 1) return;
      if (node.tagName === 'VIDEO') watch(node);
      for (const video of node.querySelectorAll(avatars + ", video." + pendingClass)) watch(video);
    }

    for (const event of ['emptied', 'loadstart', 'loadeddata', 'canplay', 'playing', 'error']) {
      document.addEventListener(event, ({ target: video }) => {
        if (video?.tagName !== 'VIDEO') return;
        if (event === 'emptied' || event === 'loadstart') watch(video, true);
        else {
          watch(video);
          const state = tracked.get(video);
          if (event === 'error' && state) show(video, state);
          else awaitFrame(video, state);
        }
      }, true);
    }

    installStyle();
    scan(document.documentElement ?? document);
    new MutationObserver((changes) => {
      installStyle();
      for (const change of changes) {
        const target = change.target;
        if (change.type === 'attributes') {
          if (change.attributeName === 'src' && target.tagName === 'VIDEO') watch(target, true);
          else if (change.attributeName === 'src' && target.parentElement?.tagName === 'VIDEO') watch(target.parentElement, true);
          else scan(target);
        } else {
          for (const node of change.addedNodes) scan(node);
          if (target.tagName === 'VIDEO' && [...change.addedNodes, ...change.removedNodes].some(node => node.tagName === 'SOURCE')) {
            watch(target, true);
          }
        }
      }
    }).observe(document, { subtree: true, childList: true, attributes: true,
      attributeFilter: ['src', 'autoplay', 'playsinline', 'data-hatch-avatar-host', 'data-hatch-media-owner'] });
  })();
  """#
}
