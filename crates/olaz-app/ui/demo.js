// Built-in standalone demo state & mock command responder for Olaz Linux.
// Active only when window.webkit?.messageHandlers?.olaz_ui is absent so the
// UI can be inspected standalone in headless Chromium or development browsers.

function svgDataUri(svg) {
  return 'data:image/svg+xml;utf8,' + encodeURIComponent(svg.trim());
}

function createDemoWavDataUri() {
  const sampleRate = 8000;
  const seconds = 2;
  const numSamples = sampleRate * seconds;
  const bytes = new Uint8Array(44 + numSamples);
  const view = new DataView(bytes.buffer);
  const writeAscii = (offset, str) => {
    for (let i = 0; i < str.length; i++) bytes[offset + i] = str.charCodeAt(i);
  };
  writeAscii(0, 'RIFF');
  view.setUint32(4, 36 + numSamples, true);
  writeAscii(8, 'WAVE');
  writeAscii(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate, true);
  view.setUint16(32, 1, true);
  view.setUint16(34, 8, true);
  writeAscii(36, 'data');
  view.setUint32(40, numSamples, true);
  for (let i = 0; i < numSamples; i++) {
    const t = i / sampleRate;
    const env = Math.exp(-t * 1.4);
    const wave = Math.sin(2 * Math.PI * 440 * t) * 0.6 + Math.sin(2 * Math.PI * 660 * t) * 0.3;
    bytes[44 + i] = Math.max(0, Math.min(255, Math.round(128 + wave * env * 48)));
  }
  let binary = '';
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return 'data:audio/wav;base64,' + btoa(binary);
}

const DEMO_PHOTO_SVG = svgDataUri(`
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 380">
  <defs>
    <linearGradient id="sky" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#0f2544"/>
      <stop offset="55%" stop-color="#1c4b82"/>
      <stop offset="100%" stop-color="#3b82c4"/>
    </linearGradient>
  </defs>
  <rect width="640" height="380" fill="url(#sky)"/>
  <circle cx="508" cy="88" r="38" fill="#f8d47c" opacity="0.92"/>
  <path d="M0 280 L145 178 L272 256 L418 148 L560 248 L640 212 L640 380 L0 380 Z" fill="#132e53"/>
  <path d="M0 312 L196 226 L342 296 L496 206 L640 284 L640 380 L0 380 Z" fill="#0b1d36"/>
  <rect x="36" y="34" width="196" height="34" rx="8" fill="#091526" opacity="0.72"/>
  <text x="52" y="56" fill="#dce9fa" font-family="sans-serif" font-size="14" font-weight="600">Hyprland + WebKit2GTK</text>
</svg>
`);

const DEMO_QR_SVG = svgDataUri(`
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 220 220">
  <rect width="220" height="220" rx="12" fill="#ffffff"/>
  <g fill="#0f1a2a">
    <rect x="20" y="20" width="54" height="54" rx="6"/>
    <rect x="28" y="28" width="38" height="38" rx="3" fill="#ffffff"/>
    <rect x="36" y="36" width="22" height="22" rx="2"/>
    <rect x="146" y="20" width="54" height="54" rx="6"/>
    <rect x="154" y="28" width="38" height="38" rx="3" fill="#ffffff"/>
    <rect x="162" y="36" width="22" height="22" rx="2"/>
    <rect x="20" y="146" width="54" height="54" rx="6"/>
    <rect x="28" y="154" width="38" height="38" rx="3" fill="#ffffff"/>
    <rect x="36" y="162" width="22" height="22" rx="2"/>
    <rect x="90" y="24" width="12" height="12"/><rect x="114" y="24" width="16" height="12"/>
    <rect x="90" y="48" width="24" height="12"/><rect x="122" y="60" width="12" height="24"/>
    <rect x="24" y="92" width="24" height="12"/><rect x="60" y="92" width="16" height="24"/>
    <rect x="92" y="92" width="36" height="36" rx="6" fill="#005be0"/>
    <rect x="102" y="102" width="16" height="16" rx="3" fill="#ffffff"/>
    <rect x="144" y="92" width="16" height="12"/><rect x="172" y="92" width="24" height="24"/>
    <rect x="90" y="146" width="20" height="16"/><rect x="122" y="146" width="16" height="28"/>
    <rect x="150" y="146" width="46" height="14"/><rect x="150" y="172" width="20" height="24"/>
    <rect x="180" y="172" width="16" height="24"/>
  </g>
</svg>
`);

function makeStickerSvg(bg, accent, label) {
  return svgDataUri(`
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
      <rect x="10" y="10" width="108" height="108" rx="28" fill="${bg}" stroke="${accent}" stroke-width="4"/>
      <circle cx="46" cy="52" r="6" fill="#0f1a2a"/>
      <circle cx="82" cy="52" r="6" fill="#0f1a2a"/>
      <path d="M44 72 Q64 88 84 72" fill="none" stroke="#0f1a2a" stroke-width="5" stroke-linecap="round"/>
      <text x="64" y="104" text-anchor="middle" fill="#0f1a2a" font-family="sans-serif" font-size="14" font-weight="700">${label}</text>
    </svg>
  `);
}

export const DEMO_STICKERS = [
  { id: 'stk-hello', cateId: 'cat-zalo', keyword: 'xin chao', label: 'Xin chào', url: makeStickerSvg('#d8ebff', '#005be0', 'Xin chào!') },
  { id: 'stk-thanks', cateId: 'cat-zalo', keyword: 'cam on', label: 'Cảm ơn', url: makeStickerSvg('#dcfce7', '#0e7a46', 'Cảm ơn') },
  { id: 'stk-ok', cateId: 'cat-zalo', keyword: 'ok chot', label: 'Chốt nhé', url: makeStickerSvg('#fef3c7', '#b45309', 'Chốt nhé') },
  { id: 'stk-cheer', cateId: 'cat-zalo', keyword: 'tuyet voi', label: 'Tuyệt vời', url: makeStickerSvg('#ede9fe', '#6d28d9', 'Quá đỉnh') },
  { id: 'stk-coffee', cateId: 'cat-dev', keyword: 'ca phe', label: 'Cà phê', url: makeStickerSvg('#ffedd5', '#c2410c', 'Cà phê?') },
  { id: 'stk-ship', cateId: 'cat-dev', keyword: 'ship deploy', label: 'Triển khai', url: makeStickerSvg('#e0f2fe', '#0369a1', 'Lên bản mới') },
];

export function createDemoState() {
  const now = Date.now();
  const voiceUri = createDemoWavDataUri();

  return {
    phase: 'ready',
    runtimeProgress: {
      step: 'downloading',
      received: 186 * 1024 * 1024,
      total: 340 * 1024 * 1024,
      failure: null,
    },
    login: {
      qr: DEMO_QR_SVG,
      invalid: false,
      avatar: '',
      text: 'Mở ứng dụng Zalo trên điện thoại và quét mã QR để đăng nhập an toàn trên máy tính Linux.',
    },
    user: {
      id: 'u-self',
      name: 'Khoa Võ',
      phone: '+84 90 318 2048',
      avatar: '',
      online: true,
    },
    convExtras: {
      inboxTitle: 'Tin nhắn',
      archivedEnabled: true,
      sendToMeId: 'conv-cloud',
      strangerIds: ['conv-stranger'],
      otherIds: ['conv-oa'],
      labels: [
        { id: 'lbl-work', name: 'Công việc', color: '#005be0' },
        { id: 'lbl-design', name: 'Thiết kế', color: '#0e7a46' },
        { id: 'lbl-urgent', name: 'Khẩn cấp', color: '#c81e2b' },
      ],
    },
    convActions: {
      hidden: {
        enabled: true,
        hasPin: true,
        ids: ['conv-hidden'],
      },
      flags: {
        'conv-minh-anh': 1,
        'conv-team': 1,
        'conv-oa': 2,
        'conv-hidden': 4,
      },
    },
    conversations: [
      {
        id: 'conv-team',
        name: 'Nhóm Thiết kế Olaz Linux',
        isGroup: true,
        members: 8,
        pinned: true,
        isMuted: false,
        isE2EE: false,
        unread: 0,
        labelId: 'lbl-work',
        memberList: [
          { id: 'all', name: 'All', isAll: true },
          { id: 'u-minh-anh', name: 'Minh Anh', isAll: false },
          { id: 'u-hoang-nam', name: 'Hoàng Nam', isAll: false },
          { id: 'u-thao-vy', name: 'Thảo Vy', isAll: false },
          { id: 'u-quoc-bao', name: 'Quốc Bảo', isAll: false },
        ],
        preview: {
          text: 'Hoàng Nam: Mình vừa cập nhật bảng bình chọn lịch phát hành bản 0.4 nhé.',
          ts: now - 3 * 60 * 1000,
          fromMe: false,
        },
      },
      {
        id: 'conv-minh-anh',
        name: 'Minh Anh',
        isGroup: false,
        members: 2,
        pinned: true,
        isMuted: false,
        isE2EE: true,
        online: true,
        unread: 2,
        labelId: 'lbl-design',
        preview: {
          text: 'Bản chụp trên Hyprland sắc nét lắm, độ tương phản đạt chuẩn luôn.',
          ts: now - 14 * 60 * 1000,
          fromMe: false,
        },
      },
      {
        id: 'conv-cloud',
        name: 'Cloud của tôi',
        isGroup: false,
        isSelfCloud: true,
        members: 1,
        pinned: false,
        isMuted: false,
        isE2EE: false,
        unread: 0,
        labelId: '',
        preview: {
          text: 'Bạn: checklist-wayland-portal-v2.pdf',
          ts: now - 72 * 60 * 1000,
          fromMe: true,
        },
      },
      {
        id: 'conv-quoc-bao',
        name: 'Quốc Bảo',
        isGroup: false,
        members: 2,
        pinned: false,
        isMuted: false,
        isE2EE: true,
        online: false,
        unread: 1,
        labelId: 'lbl-urgent',
        preview: {
          text: 'Nhờ cậu kiểm tra lại bộ gõ tiếng Việt fcitx5-bamboo khi nhấn Enter.',
          ts: now - 3 * 3600 * 1000,
          fromMe: false,
        },
      },
      {
        id: 'conv-oa',
        name: 'Hệ thống Hạ tầng Máy chủ',
        isGroup: true,
        members: 24,
        pinned: false,
        isMuted: true,
        isE2EE: false,
        unread: 0,
        labelId: '',
        preview: {
          text: 'Báo cáo định kỳ hoàn tất lúc 06:00 sáng nay.',
          ts: now - 7 * 3600 * 1000,
          fromMe: false,
        },
      },
      {
        id: 'conv-stranger',
        name: 'Lê Thanh Tùng',
        isGroup: false,
        members: 2,
        pinned: false,
        isMuted: false,
        isE2EE: false,
        unread: 1,
        labelId: '',
        preview: {
          text: 'Chào bạn, mình thấy dự án Olaz trên Arch AUR nên nhắn hỏi thăm.',
          ts: now - 12 * 3600 * 1000,
          fromMe: false,
        },
      },
      {
        id: 'conv-hidden',
        name: 'Kế hoạch Ngân sách Q4',
        isGroup: false,
        members: 2,
        pinned: false,
        isMuted: false,
        isE2EE: true,
        unread: 0,
        labelId: '',
        preview: {
          text: 'Tài liệu đối soát bảo mật bằng mã PIN.',
          ts: now - 26 * 3600 * 1000,
          fromMe: true,
        },
      },
    ],
    activeConvId: 'conv-team',
    pinsByConv: {
      'conv-team': [
        {
          ref: 'pin-1',
          msgId: 'm-team-1',
          kind: 'poll',
          title: 'Chọn ngày đóng gói gói cài đặt Arch PKGBUILD & Ubuntu .deb?',
          sender: 'Hoàng Nam',
        },
        {
          ref: 'pin-2',
          msgId: 'm-team-2',
          kind: 'text',
          title: 'Quy chuẩn giao diện Omarchy: đồng bộ --radius và bảng màu hệ thống theo thời gian thực.',
          sender: 'Khoa Võ',
        },
      ],
      'conv-minh-anh': [
        {
          ref: 'pin-ma-1',
          msgId: 'm-ma-1',
          kind: 'file',
          title: 'olaz-webkit2gtk-architecture.pdf',
          sender: 'Khoa Võ',
        },
      ],
    },
    typingByConv: {
      'conv-team': 'Thảo Vy',
    },
    polls: {
      'poll-release-04': {
        id: 'poll-release-04',
        groupId: 'conv-team',
        question: 'Chọn ngày đóng gói gói cài đặt Arch PKGBUILD & Ubuntu .deb?',
        creatorName: 'Hoàng Nam',
        created: now - 3600 * 1000,
        closed: false,
        multi: true,
        allowAdd: true,
        anonymous: false,
        voters: 6,
        options: [
          { id: 'opt-1', content: 'Thứ Năm tuần này (ưu tiên bản Omarchy)', votes: 5, voted: true },
          { id: 'opt-2', content: 'Thứ Sáu sau khi kiểm thử WebKit2GTK 2.46', votes: 3, voted: false },
          { id: 'opt-3', content: 'Đầu tuần sau kèm gói Flatpak', votes: 1, voted: false },
        ],
      },
    },
    messagesByConv: {
      'conv-team': {
        hasMore: true,
        items: [
          {
            id: 'm-team-0',
            conversationId: 'conv-team',
            senderId: 'sys',
            senderName: 'Hệ thống',
            isMine: false,
            kind: { type: 'system' },
            text: 'Khoa Võ đã bật đồng bộ giao diện trực tiếp với Omarchy Linux',
            date: now - 7200 * 1000,
            status: 'seen',
            reactions: [],
          },
          {
            id: 'm-team-2',
            conversationId: 'conv-team',
            senderId: 'u-self',
            senderName: 'Khoa Võ',
            isMine: true,
            kind: { type: 'text' },
            text: '@All Quy chuẩn giao diện Omarchy: đồng bộ --radius (0px trên Hyprland vuông góc, 12px trên GNOME) và hỗ trợ đầy đủ bộ gõ fcitx5-bamboo nhé.',
            mentions: [{ uid: 'all', pos: 0, len: 4, isAll: true }],
            date: now - 5400 * 1000,
            status: 'seen',
            isE2EE: false,
            pinned: true,
            reactions: [
              { emoji: '❤️', count: 4, includesMe: false, names: ['Minh Anh', 'Hoàng Nam', 'Thảo Vy'], people: 4 },
              { emoji: '👍', count: 2, includesMe: true, names: ['Bạn', 'Quốc Bảo'], people: 2 },
            ],
          },
          {
            id: 'm-team-photo',
            conversationId: 'conv-team',
            senderId: 'u-minh-anh',
            senderName: 'Minh Anh',
            isMine: false,
            kind: { type: 'photo', url: DEMO_PHOTO_SVG, width: 640, height: 380 },
            text: 'Ảnh chụp thử nghiệm khung cửa sổ WebKit2GTK trên màn hình 2K ở chế độ tối.',
            date: now - 2400 * 1000,
            status: 'seen',
            reactions: [{ emoji: '😮', count: 2, includesMe: true, names: ['Bạn', 'Hoàng Nam'], people: 2 }],
          },
          {
            id: 'm-team-voice',
            conversationId: 'conv-team',
            senderId: 'u-thao-vy',
            senderName: 'Thảo Vy',
            isMine: false,
            kind: { type: 'voice', url: voiceUri, duration: 2 },
            text: '[Tin nhắn thoại]',
            date: now - 1500 * 1000,
            status: 'seen',
            reactions: [],
          },
          {
            id: 'm-team-file',
            conversationId: 'conv-team',
            senderId: 'u-self',
            senderName: 'Khoa Võ',
            isMine: true,
            kind: {
              type: 'file',
              name: 'olaz-linux-bridge-spec-v1.pdf',
              size: 1482910,
              path: '/home/khoavo/Downloads/olaz-linux-bridge-spec-v1.pdf',
            },
            text: '[Tệp] olaz-linux-bridge-spec-v1.pdf',
            quote: {
              senderName: 'Minh Anh',
              text: 'Ảnh chụp thử nghiệm khung cửa sổ WebKit2GTK trên màn hình 2K ở chế độ tối.',
            },
            date: now - 900 * 1000,
            status: 'seen',
            reactions: [{ emoji: '👍', count: 3, includesMe: false, names: ['Minh Anh', 'Hoàng Nam', 'Thảo Vy'], people: 3 }],
          },
          {
            id: 'm-team-1',
            conversationId: 'conv-team',
            senderId: 'u-hoang-nam',
            senderName: 'Hoàng Nam',
            isMine: false,
            kind: {
              type: 'poll',
              pollId: 'poll-release-04',
              question: 'Chọn ngày đóng gói gói cài đặt Arch PKGBUILD & Ubuntu .deb?',
              header: 'Hoàng Nam đã tạo bình chọn mới',
            },
            text: 'Mình vừa cập nhật bảng bình chọn lịch phát hành bản 0.4 nhé.',
            date: now - 180 * 1000,
            status: 'seen',
            reactions: [],
          },
        ],
      },
      'conv-minh-anh': {
        hasMore: true,
        items: [
          {
            id: 'm-ma-1',
            conversationId: 'conv-minh-anh',
            senderId: 'u-self',
            senderName: 'Khoa Võ',
            isMine: true,
            kind: {
              type: 'file',
              name: 'olaz-webkit2gtk-architecture.pdf',
              size: 842100,
              path: '/home/khoavo/Documents/olaz-webkit2gtk-architecture.pdf',
            },
            text: 'Tài liệu kiến trúc luồng giải mã E2EE Signal đây nhé.',
            date: now - 3600 * 1000,
            status: 'seen',
            isE2EE: true,
            reactions: [],
          },
          {
            id: 'm-ma-2',
            conversationId: 'conv-minh-anh',
            senderId: 'u-minh-anh',
            senderName: 'Minh Anh',
            isMine: false,
            kind: {
              type: 'link',
              title: 'Chuẩn giao diện hiện đại cho ứng dụng Linux Wayland',
              url: 'https://wayland.freedesktop.org/docs/html/',
              source: 'wayland.freedesktop.org',
              description: 'Tài liệu tham chiếu tích hợp XDG Desktop Portal cho chụp ảnh màn hình và chọn tệp gốc.',
            },
            text: 'https://wayland.freedesktop.org/docs/html/',
            date: now - 1800 * 1000,
            status: 'seen',
            isE2EE: true,
            reactions: [{ emoji: '❤️', count: 1, includesMe: true, names: ['Bạn'], people: 1 }],
          },
          {
            id: 'm-ma-3',
            conversationId: 'conv-minh-anh',
            senderId: 'u-minh-anh',
            senderName: 'Minh Anh',
            isMine: false,
            kind: { type: 'text' },
            text: 'Bản chụp trên Hyprland sắc nét lắm, độ tương phản đạt chuẩn luôn.',
            date: now - 840 * 1000,
            status: 'seen',
            isE2EE: true,
            reactions: [],
          },
        ],
      },
      'conv-cloud': {
        hasMore: false,
        items: [
          {
            id: 'm-cloud-1',
            conversationId: 'conv-cloud',
            senderId: 'u-self',
            senderName: 'Bạn',
            isMine: true,
            kind: { type: 'text' },
            text: 'Ghi chú nhanh: Phím tắt Ctrl+K mở chuyển nhanh hội thoại, Ctrl+V dán ảnh trực tiếp từ clipboard Wayland.',
            date: now - 5400 * 1000,
            status: 'seen',
            reactions: [],
          },
          {
            id: 'm-cloud-2',
            conversationId: 'conv-cloud',
            senderId: 'u-self',
            senderName: 'Bạn',
            isMine: true,
            kind: {
              type: 'file',
              name: 'checklist-wayland-portal-v2.pdf',
              size: 512400,
              path: '/home/khoavo/Documents/checklist-wayland-portal-v2.pdf',
            },
            text: '[Tệp] checklist-wayland-portal-v2.pdf',
            date: now - 4320 * 1000,
            status: 'seen',
            reactions: [],
          },
        ],
      },
      'conv-quoc-bao': {
        hasMore: false,
        items: [
          {
            id: 'm-qb-1',
            conversationId: 'conv-quoc-bao',
            senderId: 'u-quoc-bao',
            senderName: 'Quốc Bảo',
            isMine: false,
            kind: { type: 'text' },
            text: 'Nhờ cậu kiểm tra lại bộ gõ tiếng Việt fcitx5-bamboo khi nhấn Enter.',
            date: now - 3 * 3600 * 1000,
            status: 'seen',
            isE2EE: true,
            reactions: [],
          },
        ],
      },
      'conv-stranger': {
        hasMore: false,
        items: [
          {
            id: 'm-str-1',
            conversationId: 'conv-stranger',
            senderId: 'u-stranger',
            senderName: 'Lê Thanh Tùng',
            isMine: false,
            kind: { type: 'text' },
            text: 'Chào bạn, mình thấy dự án Olaz trên Arch AUR nên nhắn hỏi thăm.',
            date: now - 12 * 3600 * 1000,
            status: 'seen',
            reactions: [],
          },
        ],
      },
      'conv-hidden': {
        hasMore: false,
        items: [
          {
            id: 'm-hid-1',
            conversationId: 'conv-hidden',
            senderId: 'u-self',
            senderName: 'Khoa Võ',
            isMine: true,
            kind: { type: 'text' },
            text: 'Tài liệu đối soát bảo mật bằng mã PIN (demo PIN: 1234).',
            date: now - 26 * 3600 * 1000,
            status: 'seen',
            isE2EE: true,
            reactions: [],
          },
        ],
      },
    },
    contacts: {
      friends: [
        { id: 'u-minh-anh', name: 'Minh Anh', zaloName: 'Minh Anh', online: true, lastActive: now, convId: 'conv-minh-anh' },
        { id: 'u-hoang-nam', name: 'Hoàng Nam', zaloName: 'Nam Hoàng', online: true, lastActive: now - 120000, convId: 'conv-team' },
        { id: 'u-thao-vy', name: 'Thảo Vy', zaloName: 'Vy Nguyễn', online: false, lastActive: now - 1800000, convId: 'conv-team' },
        { id: 'u-quoc-bao', name: 'Quốc Bảo', zaloName: 'Bảo Trần', online: false, lastActive: now - 7200000, convId: 'conv-quoc-bao' },
      ],
      groups: [
        { id: 'conv-team', name: 'Nhóm Thiết kế Olaz Linux', members: 8 },
        { id: 'conv-oa', name: 'Hệ thống Hạ tầng Máy chủ', members: 24 },
      ],
      received: [
        {
          id: 'req-1',
          name: 'Phạm Hải Đăng',
          message: 'Chào Khoa, mình cùng nhóm phát triển ứng dụng Rust trên Linux.',
          time: now - 5400 * 1000,
        },
      ],
      sent: [
        {
          id: 'req-sent-1',
          name: 'Nguyễn Lan Hương',
          message: 'Xin chào Lan Hương, kết bạn trao đổi tài liệu thiết kế nhé.',
          time: now - 86400 * 1000,
        },
      ],
      pendingRequestCount: 1,
      search: { status: 'idle', user: null, error: '' },
    },
    cloud: {
      plan: {
        kind: 'paid',
        name: 'zCloud Plus 100 GB',
        quota: 100 * 1024 * 1024 * 1024,
        usage: 28.4 * 1024 * 1024 * 1024,
      },
      types: {
        media: {
          photo: 9.2 * 1024 * 1024 * 1024,
          video: 12.6 * 1024 * 1024 * 1024,
          file: 5.8 * 1024 * 1024 * 1024,
          voice: 0.8 * 1024 * 1024 * 1024,
        },
      },
      conversations: [
        { id: 'conv-team', name: 'Nhóm Thiết kế Olaz Linux', isGroup: true, total: 11.4 * 1024 * 1024 * 1024, lastTs: now - 180000 },
        { id: 'conv-minh-anh', name: 'Minh Anh', isGroup: false, total: 6.2 * 1024 * 1024 * 1024, lastTs: now - 840000 },
        { id: 'conv-cloud', name: 'Cloud của tôi', isGroup: false, total: 4.9 * 1024 * 1024 * 1024, lastTs: now - 4320000 },
      ],
      settings: {
        offloadEnabled: true,
        ttlDays: 30,
      },
      selfId: 'conv-cloud',
    },
    sharedMediaByConv: {
      'conv-team': {
        images: [
          { id: 'sm-1', title: 'Ảnh chụp WebKit2GTK 2K', url: DEMO_PHOTO_SVG, date: now - 2400 * 1000 },
        ],
        files: [
          { id: 'sm-f1', name: 'olaz-linux-bridge-spec-v1.pdf', size: 1482910, path: '/home/khoavo/Downloads/olaz-linux-bridge-spec-v1.pdf', date: now - 900 * 1000 },
        ],
        links: [
          { id: 'sm-l1', title: 'Tài liệu XDG Desktop Portal', url: 'https://flatpak.github.io/xdg-desktop-portal/', source: 'flatpak.github.io', date: now - 86400 * 1000 },
        ],
      },
      'conv-minh-anh': {
        images: [],
        files: [
          { id: 'sm-ma-f1', name: 'olaz-webkit2gtk-architecture.pdf', size: 842100, path: '/home/khoavo/Documents/olaz-webkit2gtk-architecture.pdf', date: now - 3600 * 1000 },
        ],
        links: [
          { id: 'sm-ma-l1', title: 'Chuẩn giao diện hiện đại cho ứng dụng Linux Wayland', url: 'https://wayland.freedesktop.org/docs/html/', source: 'wayland.freedesktop.org', date: now - 1800 * 1000 },
        ],
      },
    },
    transfer: null,
    stickers: {
      categories: [
        { id: 'cat-zalo', name: 'Chào hỏi' },
        { id: 'cat-dev', name: 'Công việc' },
      ],
      items: DEMO_STICKERS,
      suggestions: [],
    },
    settings: {
      theme: 'system',
      language: 'vi',
      notifications: true,
      sound: true,
      soundVolume: 80,
      enterToSend: true,
      stickerSuggest: true,
      autoDownloadMb: 25,
      omarchySync: true,
    },
    toast: null,
    incomingCall: null,
  };
}
