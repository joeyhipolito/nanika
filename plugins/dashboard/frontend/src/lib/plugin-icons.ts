import type React from 'react'
import { ChatSquare } from '@solar-icons/react-perf/category/messages/LineDuotone'
import { Plain } from '@solar-icons/react-perf/category/messages/LineDuotone'
import { LetterOpened } from '@solar-icons/react-perf/category/messages/LineDuotone'
import { ChatRoundDots } from '@solar-icons/react-perf/category/messages/LineDuotone'
import { PenNewSquare } from '@solar-icons/react-perf/category/messages/LineDuotone'
import { Microphone } from '@solar-icons/react-perf/category/video/LineDuotone'
import { PlayCircle } from '@solar-icons/react-perf/category/video/LineDuotone'
import { Like } from '@solar-icons/react-perf/category/like/LineDuotone'
import { UserId } from '@solar-icons/react-perf/category/users/LineDuotone'
import { MagniferZoomIn } from '@solar-icons/react-perf/category/search/LineDuotone'
import { MinimalisticMagnifer } from '@solar-icons/react-perf/category/search/LineDuotone'
import { Notebook } from '@solar-icons/react-perf/category/notes/LineDuotone'
import { Calendar } from '@solar-icons/react-perf/category/time/LineDuotone'
import { ListCheck } from '@solar-icons/react-perf/category/list/LineDuotone'
import { Wallet } from '@solar-icons/react-perf/category/money/LineDuotone'

type IconComponent = React.ComponentType<{ size?: number | string }>

// Map from icon string names (used in plugin.json "icon" field) to Solar icon
// components. Keys use the Solar component name exactly so plugin authors can
// reference the Solar icon catalogue directly.
export const iconMap: Record<string, IconComponent> = {
  // messages
  ChatSquare,
  Plain,
  LetterOpened,
  ChatRoundDots,
  PenNewSquare,
  // video / audio
  Microphone,
  PlayCircle,
  // like / social
  Like,
  // users
  UserId,
  // search
  MagniferZoomIn,
  MinimalisticMagnifer,
  // notes
  Notebook,
  // time
  Calendar,
  // list
  ListCheck,
  // money
  Wallet,
}
