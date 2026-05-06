// Single source of truth for every icon used in whim-web.
// Adding an icon: import here from `~icons/solar/<name>-<variant>` and re-export
// under a PascalCase semantic name. Components consume the named export through
// the `<Icon>` wrapper (see `./Icon.tsx`) — they never import the Solar path
// directly. Naming is by role (e.g. `ChevronRight` — chevron used as a
// right-affordance), not by glyph shape.

import IconSearchLinear from '~icons/solar/magnifer-linear';
import IconMicLinear from '~icons/solar/microphone-linear';
import IconMicBoldDuotone from '~icons/solar/microphone-bold-duotone';
import IconChevronRightLinear from '~icons/solar/alt-arrow-right-linear';
import IconChevronLeftLinear from '~icons/solar/alt-arrow-left-linear';
import IconArrowUpLinear from '~icons/solar/alt-arrow-up-linear';
import IconArrowDownLinear from '~icons/solar/alt-arrow-down-linear';
import IconArrowLeftLinear from '~icons/solar/arrow-left-linear';
import IconArrowRightLinear from '~icons/solar/arrow-right-linear';
import IconSendBoldDuotone from '~icons/solar/plain-bold-duotone';
import IconBoltLinear from '~icons/solar/bolt-linear';
import IconBoltBoldDuotone from '~icons/solar/bolt-bold-duotone';
import IconCheckCircleLinear from '~icons/solar/check-circle-linear';
import IconRecordCircleLinear from '~icons/solar/record-circle-linear';
import IconChatLinear from '~icons/solar/chat-round-dots-linear';
import IconChatBoldDuotone from '~icons/solar/chat-round-dots-bold-duotone';
import IconChecklistLinear from '~icons/solar/checklist-minimalistic-linear';
import IconChecklistBoldDuotone from '~icons/solar/checklist-minimalistic-bold-duotone';
import IconCodeLinear from '~icons/solar/code-square-linear';
import IconCodeBoldDuotone from '~icons/solar/code-square-bold-duotone';
import IconAddCircleLinear from '~icons/solar/add-circle-linear';
import IconAddCircleBoldDuotone from '~icons/solar/add-circle-bold-duotone';
import IconAddSquareLinear from '~icons/solar/add-square-linear';
import IconCustomizeLinear from '~icons/solar/settings-linear';
import IconPinLinear from '~icons/solar/pin-linear';
import IconExpandWindowLinear from '~icons/solar/maximize-square-linear';
import IconLockLinear from '~icons/solar/lock-keyhole-linear';
import IconStarsBoldDuotone from '~icons/solar/stars-minimalistic-bold-duotone';
import IconAttachLinear from '~icons/solar/paperclip-linear';
import IconPullRequestBoldDuotone from '~icons/solar/branching-paths-down-bold-duotone';
import IconSortAscLinear from '~icons/solar/sort-from-top-to-bottom-linear';
import IconSortDescLinear from '~icons/solar/sort-from-bottom-to-top-linear';
import IconListLinear from '~icons/solar/list-linear';
import IconWidgetLinear from '~icons/solar/widget-2-linear';
import IconCloseCircleLinear from '~icons/solar/close-circle-linear';
import IconFolderLinear from '~icons/solar/folder-linear';
import IconFolderOpenLinear from '~icons/solar/folder-open-linear';
import IconFileLinear from '~icons/solar/file-linear';
import IconFileTextLinear from '~icons/solar/document-text-linear';
import IconEyeLinear from '~icons/solar/eye-linear';
import IconEyeBoldDuotone from '~icons/solar/eye-bold-duotone';
import IconSidebarLinear from '~icons/solar/sidebar-minimalistic-linear';
import IconCarouselVerticalLinear from '~icons/solar/posts-carousel-vertical-linear';
import IconTrashLinear from '~icons/solar/trash-bin-trash-linear';
import IconBranchLinear from '~icons/solar/branching-paths-up-linear';
import IconAlertLinear from '~icons/solar/danger-triangle-linear';
import IconFilterLinear from '~icons/solar/tuning-square-linear';
import IconMoreLinear from '~icons/solar/menu-dots-linear';
import IconMoreVerticalLinear from '~icons/solar/menu-dots-square-linear';
import IconUserLinear from '~icons/solar/user-linear';
import IconFileBoldDuotone from '~icons/solar/file-bold-duotone';
import IconGitDiffLinear from '~icons/solar/code-2-linear';
import IconGitDiffBoldDuotone from '~icons/solar/code-2-bold-duotone';
import IconTerminalLinear from '~icons/solar/command-linear';
import IconBookmarkLinear from '~icons/solar/bookmark-linear';
import IconBookOpenLinear from '~icons/solar/book-2-linear';
import IconShieldWarningLinear from '~icons/solar/shield-warning-linear';
import IconShieldWarningBoldDuotone from '~icons/solar/shield-warning-bold-duotone';
import IconSparklesLinear from '~icons/solar/stars-minimalistic-linear';

import type { ComponentType, SVGProps } from 'react';

export type IconComponent = ComponentType<SVGProps<SVGSVGElement>>;

// ─── Public registry (semantic name → component) ───────────────────────────
// This is the surface every component imports from. Internal Solar import
// paths above are an implementation detail and may change without breaking
// callers as long as the semantic export shape is preserved.

export const Search: IconComponent = IconSearchLinear;
export const Mic: IconComponent = IconMicLinear;
export const MicActive: IconComponent = IconMicBoldDuotone;
export const ChevronRight: IconComponent = IconChevronRightLinear;
export const ChevronLeft: IconComponent = IconChevronLeftLinear;
export const ArrowUp: IconComponent = IconArrowUpLinear;
export const ArrowDown: IconComponent = IconArrowDownLinear;
export const CaretDown: IconComponent = IconArrowDownLinear;
export const CaretUp: IconComponent = IconArrowUpLinear;
export const ArrowLeft: IconComponent = IconArrowLeftLinear;
export const ArrowRight: IconComponent = IconArrowRightLinear;
export const Send: IconComponent = IconSendBoldDuotone;
export const BoltDefault: IconComponent = IconBoltLinear;
export const BoltActive: IconComponent = IconBoltBoldDuotone;
export const CheckDone: IconComponent = IconCheckCircleLinear;
export const CirclePending: IconComponent = IconRecordCircleLinear;
export const ModeChat: IconComponent = IconChatLinear;
export const ModeChatActive: IconComponent = IconChatBoldDuotone;
export const ModeTodo: IconComponent = IconChecklistLinear;
export const ModeTodoActive: IconComponent = IconChecklistBoldDuotone;
export const ModeCode: IconComponent = IconCodeLinear;
export const ModeCodeActive: IconComponent = IconCodeBoldDuotone;
export const Plus: IconComponent = IconAddCircleLinear;
export const PlusAccent: IconComponent = IconAddCircleBoldDuotone;
export const NewTab: IconComponent = IconAddSquareLinear;
export const Customize: IconComponent = IconCustomizeLinear;
export const Pin: IconComponent = IconPinLinear;
export const ExpandWindow: IconComponent = IconExpandWindowLinear;
export const Lock: IconComponent = IconLockLinear;
export const ModelLeading: IconComponent = IconStarsBoldDuotone;
export const Attach: IconComponent = IconAttachLinear;
export const PullRequest: IconComponent = IconPullRequestBoldDuotone;
export const SortAsc: IconComponent = IconSortAscLinear;
export const SortDesc: IconComponent = IconSortDescLinear;
export const ViewList: IconComponent = IconListLinear;
export const ViewCompact: IconComponent = IconWidgetLinear;
export const Close: IconComponent = IconCloseCircleLinear;
export const Folder: IconComponent = IconFolderLinear;
export const FolderOpen: IconComponent = IconFolderOpenLinear;
export const File: IconComponent = IconFileLinear;
export const FileText: IconComponent = IconFileTextLinear;
export const Eye: IconComponent = IconEyeLinear;
export const EyeActive: IconComponent = IconEyeBoldDuotone;
export const LayoutUnified: IconComponent = IconSidebarLinear;
export const LayoutSplit: IconComponent = IconCarouselVerticalLinear;
export const SplitWindow: IconComponent = IconCarouselVerticalLinear;
export const Trash: IconComponent = IconTrashLinear;
export const Branch: IconComponent = IconBranchLinear;
export const Alert: IconComponent = IconAlertLinear;
export const Filter: IconComponent = IconFilterLinear;
export const More: IconComponent = IconMoreLinear;
export const MoreVertical: IconComponent = IconMoreVerticalLinear;
export const User: IconComponent = IconUserLinear;
export const PanelRight: IconComponent = IconSidebarLinear;
export const Split: IconComponent = IconCarouselVerticalLinear;
export const FileActive: IconComponent = IconFileBoldDuotone;
export const GitDiff: IconComponent = IconGitDiffLinear;
export const GitDiffActive: IconComponent = IconGitDiffBoldDuotone;
export const Terminal: IconComponent = IconTerminalLinear;
export const Bookmark: IconComponent = IconBookmarkLinear;
export const BookOpen: IconComponent = IconBookOpenLinear;
export const ShieldWarning: IconComponent = IconShieldWarningLinear;
export const ShieldWarningActive: IconComponent = IconShieldWarningBoldDuotone;
export const Sparkles: IconComponent = IconSparklesLinear;

// ─── Name-keyed lookup (for `<Icon name="...">` consumer ergonomics) ───────

export const iconRegistry = {
  Search,
  Mic,
  MicActive,
  ChevronRight,
  ChevronLeft,
  ArrowUp,
  ArrowDown,
  CaretDown,
  CaretUp,
  ArrowLeft,
  ArrowRight,
  Send,
  BoltDefault,
  BoltActive,
  CheckDone,
  CirclePending,
  ModeChat,
  ModeChatActive,
  ModeTodo,
  ModeTodoActive,
  ModeCode,
  ModeCodeActive,
  Plus,
  PlusAccent,
  NewTab,
  Customize,
  Pin,
  ExpandWindow,
  Lock,
  ModelLeading,
  Attach,
  PullRequest,
  SortAsc,
  SortDesc,
  ViewList,
  ViewCompact,
  Close,
  Folder,
  FolderOpen,
  File,
  FileText,
  Eye,
  EyeActive,
  LayoutUnified,
  LayoutSplit,
  SplitWindow,
  Trash,
  Branch,
  Alert,
  Filter,
  More,
  MoreVertical,
  User,
  PanelRight,
  Split,
  FileActive,
  GitDiff,
  GitDiffActive,
  Terminal,
  Bookmark,
  BookOpen,
  ShieldWarning,
  ShieldWarningActive,
  Sparkles,
} as const satisfies Record<string, IconComponent>;

export type IconName = keyof typeof iconRegistry;
