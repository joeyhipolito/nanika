import { useState, useEffect, useCallback, useMemo } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import type { PluginViewProps } from '@/types'
import { queryPluginStatus, queryPluginItems } from '@/lib/wails'

// ---------------------------------------------------------------------------
// Domain types — mirrors ynab query status/items output
// ---------------------------------------------------------------------------

interface CategoryItem {
  category_group: string
  category: string
  budgeted_milliunits: number
  activity_milliunits: number
  balance_milliunits: number
}

interface TransactionItem {
  date: string
  payee_name: string
  category_name: string
  amount_milliunits: number
}

interface AccountItem {
  name: string
  type: string
  balance_milliunits: number
  on_budget: boolean
}

interface YnabStatus {
  budget_name: string
  month: string
  total_budgeted_milliunits: number
  total_activity_milliunits: number
  total_balance_milliunits: number
  categories?: CategoryItem[]
  transactions?: TransactionItem[]
  accounts?: AccountItem[]
}

interface OverspentCategory {
  category_group: string
  category: string
  balance_milliunits: number
}

interface NewTransactionForm {
  date: string
  payee: string
  amount: string
  categoryName: string
  accountName: string
  memo: string
  isInflow: boolean
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatCurrency(milliunits: number): string {
  const dollars = milliunits / 1000
  const abs = Math.abs(dollars)
  const formatted = abs.toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
  return dollars < 0 ? `-$${formatted}` : `$${formatted}`
}

function formatMonth(isoMonth: string): string {
  const parts = isoMonth.split('-')
  if (parts.length < 2) return isoMonth
  const year = parseInt(parts[0] ?? '0', 10)
  const month = parseInt(parts[1] ?? '0', 10) - 1
  return new Date(year, month, 1).toLocaleDateString('en-US', { month: 'long', year: 'numeric' })
}

function formatDate(iso: string): string {
  const parts = iso.split('-')
  if (parts.length < 3) return iso
  const month = parseInt(parts[1] ?? '0', 10)
  const day = parseInt(parts[2] ?? '0', 10)
  return `${month}/${day}`
}

function accountTypeLabel(type: string): string {
  switch (type) {
    case 'checking': return 'Checking'
    case 'savings': return 'Savings'
    case 'creditCard': return 'Credit'
    case 'cash': return 'Cash'
    case 'lineOfCredit': return 'LOC'
    case 'otherAsset': return 'Asset'
    case 'otherLiability': return 'Liability'
    default: return type
  }
}

function todayISO(): string {
  return new Date().toISOString().split('T')[0] ?? ''
}

// ---------------------------------------------------------------------------
// MonthSelector — display-only; multi-month browsing requires backend support
// ---------------------------------------------------------------------------

function MonthSelector({ month }: { month: string }) {
  return (
    <div className="flex items-center gap-1 mb-3" aria-label={`Current month: ${formatMonth(month)}`}>
      <Button variant="ghost" size="icon" className="h-6 w-6" disabled aria-label="Previous month">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <path d="m15 18-6-6 6-6" />
        </svg>
      </Button>
      <span className="text-[13px] font-semibold px-1" style={{ color: 'var(--text-primary)' }}>
        {formatMonth(month)}
      </span>
      <Button variant="ghost" size="icon" className="h-6 w-6" disabled aria-label="Next month">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <path d="m9 18 6-6-6-6" />
        </svg>
      </Button>
    </div>
  )
}

// ---------------------------------------------------------------------------
// BudgetSummaryCard
// ---------------------------------------------------------------------------

function BudgetSummaryCard({ status }: { status: YnabStatus }) {
  const spent = Math.abs(status.total_activity_milliunits)
  const budgeted = status.total_budgeted_milliunits
  const remaining = status.total_balance_milliunits
  const spentPct = budgeted > 0 ? Math.min((spent / budgeted) * 100, 100) : 0
  const isOverBudget = remaining < 0

  return (
    <section aria-label="Budget summary">
      <MonthSelector month={status.month} />
      <p className="text-[10px] uppercase tracking-wider font-medium mb-2" style={{ color: 'var(--text-secondary)' }}>
        {status.budget_name}
      </p>
      <Card className="p-4 flex flex-col gap-3" style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
        <div className="grid grid-cols-3 gap-2 text-center">
          <div>
            <p className="text-[10px] uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>Budgeted</p>
            <p className="text-base font-semibold font-mono tabular-nums" style={{ color: 'var(--text-primary)' }}>
              {formatCurrency(budgeted)}
            </p>
          </div>
          <div>
            <p className="text-[10px] uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>Spent</p>
            <p className="text-base font-semibold font-mono tabular-nums" style={{ color: 'var(--text-primary)' }}>
              {formatCurrency(spent)}
            </p>
          </div>
          <div>
            <p className="text-[10px] uppercase tracking-wide" style={{ color: 'var(--text-secondary)' }}>Remaining</p>
            <p
              className="text-base font-semibold font-mono tabular-nums"
              style={{ color: isOverBudget ? 'var(--color-error)' : 'var(--color-success)' }}
            >
              {formatCurrency(remaining)}
            </p>
          </div>
        </div>
        <div>
          <div className="flex justify-between mb-1">
            <span className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>
              {spentPct.toFixed(0)}% of budget used
            </span>
            {isOverBudget && (
              <Badge variant="destructive" className="text-[10px] px-1.5 py-0">over budget</Badge>
            )}
          </div>
          <div
            className="h-2 rounded-full w-full overflow-hidden"
            style={{ background: 'var(--pill-border)' }}
            role="progressbar"
            aria-valuenow={spentPct}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label={`${spentPct.toFixed(0)}% of budget spent`}
          >
            <div
              className="h-full rounded-full transition-all duration-300"
              style={{
                width: `${spentPct}%`,
                background: isOverBudget ? 'var(--color-error)' : spentPct > 80 ? '#f59e0b' : 'var(--color-success)',
              }}
            />
          </div>
        </div>
      </Card>
    </section>
  )
}

// ---------------------------------------------------------------------------
// CategoryAccordion — grouped by category_group, collapsible per group
// ---------------------------------------------------------------------------

function CategoryProgressBar({ pct, isOverspent }: { pct: number; isOverspent: boolean }) {
  return (
    <div
      className="h-1.5 rounded-full w-full overflow-hidden"
      style={{ background: 'var(--pill-border)' }}
      role="progressbar"
      aria-valuenow={pct}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <div
        className="h-full rounded-full transition-all duration-300"
        style={{
          width: `${pct}%`,
          background: isOverspent ? 'var(--color-error)' : pct > 80 ? '#f59e0b' : 'var(--color-success)',
        }}
      />
    </div>
  )
}

function CategoryRow({ cat }: { cat: CategoryItem }) {
  const spent = Math.abs(cat.activity_milliunits)
  const budgeted = cat.budgeted_milliunits
  const isOverspent = cat.balance_milliunits < 0
  const pct = budgeted > 0 ? Math.min((spent / budgeted) * 100, 100) : spent > 0 ? 100 : 0

  return (
    <div className="px-3 py-2.5" aria-label={`${cat.category}: ${formatCurrency(cat.balance_milliunits)} remaining`}>
      <div className="flex items-start justify-between mb-1.5">
        <span
          className="text-[12px] font-medium truncate flex-1 mr-2 leading-tight"
          style={{ color: isOverspent ? 'var(--color-error)' : 'var(--text-primary)' }}
        >
          {cat.category}
        </span>
        <div className="text-right flex-shrink-0">
          <span
            className="text-[12px] font-mono tabular-nums"
            style={{ color: isOverspent ? 'var(--color-error)' : 'var(--text-primary)' }}
          >
            {formatCurrency(cat.balance_milliunits)}
          </span>
          {budgeted > 0 && (
            <span className="text-[10px] block" style={{ color: 'var(--text-secondary)' }}>
              of {formatCurrency(budgeted)}
            </span>
          )}
        </div>
      </div>
      <CategoryProgressBar pct={pct} isOverspent={isOverspent} />
    </div>
  )
}

function CategoryGroupSection({ groupName, categories }: { groupName: string; categories: CategoryItem[] }) {
  const hasOverspent = categories.some(c => c.balance_milliunits < 0)
  const [open, setOpen] = useState(hasOverspent)

  const totalBalance = categories.reduce((s, c) => s + c.balance_milliunits, 0)
  const totalBudgeted = categories.reduce((s, c) => s + c.budgeted_milliunits, 0)
  const totalSpent = categories.reduce((s, c) => s + Math.abs(c.activity_milliunits), 0)
  const isGroupOverspent = totalBalance < 0
  const overspentCount = categories.filter(c => c.balance_milliunits < 0).length

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <button
          className="w-full flex items-center justify-between px-3 py-2.5 text-left transition-opacity hover:opacity-75"
          style={{ borderBottom: '1px solid var(--pill-border)' }}
          aria-expanded={open}
        >
          <div className="flex items-center gap-2 flex-1 min-w-0">
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              className={`flex-shrink-0 transition-transform duration-200 ${open ? 'rotate-180' : ''}`}
              style={{ color: 'var(--text-secondary)' }}
            >
              <path d="m6 9 6 6 6-6" />
            </svg>
            <span
              className="text-[12px] font-medium truncate"
              style={{ color: isGroupOverspent ? 'var(--color-error)' : 'var(--text-primary)' }}
            >
              {groupName}
            </span>
            {overspentCount > 0 && (
              <Badge variant="destructive" className="text-[9px] px-1 py-0 flex-shrink-0">
                {overspentCount} over
              </Badge>
            )}
          </div>
          <div className="text-right flex-shrink-0 ml-2">
            <span
              className="text-[11px] font-mono tabular-nums"
              style={{ color: isGroupOverspent ? 'var(--color-error)' : 'var(--text-secondary)' }}
            >
              {formatCurrency(totalBalance)}
            </span>
            <span className="text-[10px] block" style={{ color: 'var(--text-secondary)' }}>
              {formatCurrency(totalSpent)} / {formatCurrency(totalBudgeted)}
            </span>
          </div>
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="flex flex-col" style={{ borderBottom: '1px solid var(--pill-border)' }}>
          {categories.map((cat, i) => (
            <div
              key={`${cat.category_group}:${cat.category}`}
              style={i < categories.length - 1 ? { borderBottom: '1px solid var(--pill-border)' } : undefined}
            >
              <CategoryRow cat={cat} />
            </div>
          ))}
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}

function CategoryAccordion({ categories }: { categories: CategoryItem[] }) {
  const groups = useMemo(() => {
    const map = new Map<string, CategoryItem[]>()
    for (const cat of categories) {
      const group = map.get(cat.category_group) ?? []
      group.push(cat)
      map.set(cat.category_group, group)
    }
    return [...map.entries()]
      .map(([name, cats]) => ({ name, cats }))
      .sort((a, b) => {
        const aOver = a.cats.some(c => c.balance_milliunits < 0) ? 1 : 0
        const bOver = b.cats.some(c => c.balance_milliunits < 0) ? 1 : 0
        return bOver - aOver
      })
  }, [categories])

  return (
    <section aria-label="Category budgets">
      <h2 className="text-[10px] uppercase tracking-wider font-medium mb-2" style={{ color: 'var(--text-secondary)' }}>
        Categories
      </h2>
      <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', overflow: 'hidden' }}>
        {groups.map(({ name, cats }) => (
          <CategoryGroupSection key={name} groupName={name} categories={cats} />
        ))}
      </Card>
    </section>
  )
}

// ---------------------------------------------------------------------------
// AddTransactionDialog
// ---------------------------------------------------------------------------

const EMPTY_FORM: NewTransactionForm = {
  date: '',
  payee: '',
  amount: '',
  categoryName: '',
  accountName: '',
  memo: '',
  isInflow: false,
}

const FIELD_INPUT_CLASS =
  'w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none focus:ring-1 focus:ring-current'

function AddTransactionDialog({
  open,
  onClose,
  accounts,
  categories,
}: {
  open: boolean
  onClose: () => void
  accounts: AccountItem[]
  categories: CategoryItem[]
}) {
  const [form, setForm] = useState<NewTransactionForm>({ ...EMPTY_FORM, date: todayISO() })
  const [submitting, setSubmitting] = useState(false)

  const uniqueCategories = useMemo(
    () => [...new Set(categories.map(c => c.category))].sort(),
    [categories]
  )

  function handleClose() {
    setForm({ ...EMPTY_FORM, date: todayISO() })
    onClose()
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    setSubmitting(true)
    // DECISION: Add transaction calls `ynab add` CLI. Backend action not yet wired;
    // adding the action to plugin.json + wiring pluginAction() from @/lib/wails is the next step.
    await new Promise(r => setTimeout(r, 300))
    setSubmitting(false)
    handleClose()
  }

  const fieldStyle = {
    background: 'var(--mic-bg)',
    borderColor: 'var(--pill-border)',
    color: 'var(--text-primary)',
  }

  return (
    <Dialog open={open} onOpenChange={(v: boolean) => !v && handleClose()}>
      <DialogContent
        className="sm:max-w-md"
        style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
      >
        <DialogHeader>
          <DialogTitle style={{ color: 'var(--text-primary)' }}>Add Transaction</DialogTitle>
        </DialogHeader>

        <form onSubmit={e => void handleSubmit(e)} className="flex flex-col gap-3 mt-1">
          <div className="grid grid-cols-2 gap-2">
            <div>
              <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
                Date
              </label>
              <input
                type="date"
                required
                value={form.date}
                onChange={e => setForm(f => ({ ...f, date: e.target.value }))}
                className={FIELD_INPUT_CLASS}
                style={fieldStyle}
              />
            </div>
            <div>
              <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
                Type
              </label>
              <Select
                value={form.isInflow ? 'inflow' : 'outflow'}
                onValueChange={(v: string) => setForm(f => ({ ...f, isInflow: v === 'inflow' }))}
              >
                <SelectTrigger
                  className="h-[34px] text-[13px]"
                  style={fieldStyle}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="outflow">Outflow</SelectItem>
                  <SelectItem value="inflow">Inflow</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>

          <div>
            <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
              Payee
            </label>
            <input
              type="text"
              required
              placeholder="Who did you pay?"
              value={form.payee}
              onChange={e => setForm(f => ({ ...f, payee: e.target.value }))}
              className={FIELD_INPUT_CLASS}
              style={fieldStyle}
            />
          </div>

          <div>
            <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
              Amount ($)
            </label>
            <input
              type="number"
              required
              min="0.01"
              step="0.01"
              placeholder="0.00"
              value={form.amount}
              onChange={e => setForm(f => ({ ...f, amount: e.target.value }))}
              className={FIELD_INPUT_CLASS}
              style={fieldStyle}
            />
          </div>

          <div>
            <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
              Account
            </label>
            {accounts.length > 0 ? (
              <Select
                value={form.accountName}
                onValueChange={(v: string) => setForm(f => ({ ...f, accountName: v }))}
              >
                <SelectTrigger className="h-[34px] text-[13px]" style={fieldStyle}>
                  <SelectValue placeholder="Select account" />
                </SelectTrigger>
                <SelectContent>
                  {accounts.map(a => (
                    <SelectItem key={a.name} value={a.name}>{a.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <input
                type="text"
                placeholder="Account name"
                value={form.accountName}
                onChange={e => setForm(f => ({ ...f, accountName: e.target.value }))}
                className={FIELD_INPUT_CLASS}
                style={fieldStyle}
              />
            )}
          </div>

          <div>
            <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
              Category
            </label>
            {uniqueCategories.length > 0 ? (
              <Select
                value={form.categoryName}
                onValueChange={(v: string) => setForm(f => ({ ...f, categoryName: v }))}
              >
                <SelectTrigger className="h-[34px] text-[13px]" style={fieldStyle}>
                  <SelectValue placeholder="Select category" />
                </SelectTrigger>
                <SelectContent>
                  {uniqueCategories.map(c => (
                    <SelectItem key={c} value={c}>{c}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <input
                type="text"
                placeholder="Category name"
                value={form.categoryName}
                onChange={e => setForm(f => ({ ...f, categoryName: e.target.value }))}
                className={FIELD_INPUT_CLASS}
                style={fieldStyle}
              />
            )}
          </div>

          <div>
            <label className="text-[10px] uppercase tracking-wide block mb-1" style={{ color: 'var(--text-secondary)' }}>
              Memo <span style={{ color: 'var(--text-secondary)', textTransform: 'none', letterSpacing: 'normal' }}>(optional)</span>
            </label>
            <input
              type="text"
              placeholder="Note..."
              value={form.memo}
              onChange={e => setForm(f => ({ ...f, memo: e.target.value }))}
              className={FIELD_INPUT_CLASS}
              style={fieldStyle}
            />
          </div>

          <DialogFooter className="mt-1 gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={handleClose}>
              Cancel
            </Button>
            <Button type="submit" size="sm" disabled={submitting}>
              {submitting ? 'Adding…' : 'Add Transaction'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// TransactionTable — with search + category filter
// ---------------------------------------------------------------------------

function TransactionTable({
  transactions,
  categories,
  accounts,
  onRefresh,
}: {
  transactions: TransactionItem[]
  categories: CategoryItem[]
  accounts: AccountItem[]
  onRefresh: () => void
}) {
  const [search, setSearch] = useState('')
  const [categoryFilter, setCategoryFilter] = useState('all')
  const [addOpen, setAddOpen] = useState(false)

  const uniqueCategories = useMemo(
    () => [...new Set(transactions.map(t => t.category_name).filter(Boolean))].sort(),
    [transactions]
  )

  const filtered = useMemo(
    () =>
      transactions.filter(t => {
        const q = search.toLowerCase()
        const matchesSearch =
          !q ||
          t.payee_name.toLowerCase().includes(q) ||
          t.category_name.toLowerCase().includes(q)
        const matchesCategory = categoryFilter === 'all' || t.category_name === categoryFilter
        return matchesSearch && matchesCategory
      }),
    [transactions, search, categoryFilter]
  )

  return (
    <section aria-label="Transactions">
      <div className="flex items-center justify-between mb-2">
        <h2 className="text-[10px] uppercase tracking-wider font-medium" style={{ color: 'var(--text-secondary)' }}>
          Transactions
        </h2>
        <Button
          size="sm"
          variant="outline"
          className="h-6 text-[11px] px-2 gap-1"
          onClick={() => setAddOpen(true)}
          aria-label="Add transaction"
        >
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
            <path d="M12 5v14M5 12h14" />
          </svg>
          Add
        </Button>
      </div>

      {/* Filters */}
      <div className="flex gap-2 mb-2">
        <div className="relative flex-1">
          <svg
            width="12"
            height="12"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            className="absolute left-2 top-1/2 -translate-y-1/2 pointer-events-none"
            style={{ color: 'var(--text-secondary)' }}
          >
            <circle cx="11" cy="11" r="8" /><path d="m21 21-4.35-4.35" />
          </svg>
          <input
            type="search"
            placeholder="Search payee or category…"
            value={search}
            onChange={e => setSearch(e.target.value)}
            className="w-full rounded-md border pl-6 pr-2.5 py-1 text-[12px] outline-none focus:ring-1"
            style={{
              background: 'var(--mic-bg)',
              borderColor: 'var(--pill-border)',
              color: 'var(--text-primary)',
            }}
          />
        </div>
        {uniqueCategories.length > 1 && (
          <Select value={categoryFilter} onValueChange={setCategoryFilter}>
            <SelectTrigger
              className="h-[30px] text-[12px] w-36 flex-shrink-0"
              style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', color: 'var(--text-primary)' }}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All categories</SelectItem>
              {uniqueCategories.map(c => (
                <SelectItem key={c} value={c}>{c}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
      </div>

      {/* Table */}
      <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)', overflow: 'hidden' }}>
        <div
          className="grid gap-2 px-3 py-1.5 text-[10px] uppercase tracking-wide"
          style={{
            gridTemplateColumns: '2rem 1fr 1fr auto',
            borderBottom: '1px solid var(--pill-border)',
            background: 'color-mix(in srgb, var(--pill-border) 30%, transparent)',
            color: 'var(--text-secondary)',
          }}
        >
          <span>Date</span>
          <span>Payee</span>
          <span>Category</span>
          <span className="text-right">Amount</span>
        </div>

        {filtered.length === 0 ? (
          <div className="px-3 py-5 text-center">
            <p className="text-[12px]" style={{ color: 'var(--text-secondary)' }}>
              {transactions.length === 0 ? 'No transactions.' : 'No matches for current filters.'}
            </p>
          </div>
        ) : (
          filtered.map((txn, i) => {
            const isOutflow = txn.amount_milliunits < 0
            return (
              <div
                key={`${txn.date}:${txn.payee_name}:${txn.amount_milliunits}:${i}`}
                className="grid items-center gap-2 px-3 py-2"
                style={{
                  gridTemplateColumns: '2rem 1fr 1fr auto',
                  borderBottom: i < filtered.length - 1 ? '1px solid var(--pill-border)' : undefined,
                }}
              >
                <span className="text-[11px] font-mono" style={{ color: 'var(--text-secondary)' }}>
                  {formatDate(txn.date)}
                </span>
                <p className="text-[12px] font-medium truncate" style={{ color: 'var(--text-primary)' }}>
                  {txn.payee_name || '—'}
                </p>
                <p className="text-[11px] truncate" style={{ color: 'var(--text-secondary)' }}>
                  {txn.category_name || '—'}
                </p>
                <span
                  className="text-[12px] font-mono tabular-nums text-right"
                  style={{ color: isOutflow ? 'var(--color-error)' : 'var(--color-success)' }}
                  aria-label={`${isOutflow ? 'Outflow' : 'Inflow'}: ${formatCurrency(txn.amount_milliunits)}`}
                >
                  {formatCurrency(txn.amount_milliunits)}
                </span>
              </div>
            )
          })
        )}
      </Card>

      <AddTransactionDialog
        open={addOpen}
        onClose={() => { setAddOpen(false); onRefresh() }}
        accounts={accounts}
        categories={categories}
      />
    </section>
  )
}

// ---------------------------------------------------------------------------
// AccountBalanceCards
// ---------------------------------------------------------------------------

function AccountBalanceCards({ accounts }: { accounts: AccountItem[] }) {
  return (
    <section aria-label="Account balances">
      <h2 className="text-[10px] uppercase tracking-wider font-medium mb-2" style={{ color: 'var(--text-secondary)' }}>
        Accounts
      </h2>
      <div className="flex flex-col gap-1.5">
        {accounts.length === 0 ? (
          <Card className="p-3" style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
            <p className="text-[11px]" style={{ color: 'var(--text-secondary)' }}>No accounts found.</p>
          </Card>
        ) : (
          accounts.map(acct => {
            const isCreditLiability =
              acct.type === 'creditCard' || acct.type === 'lineOfCredit' || acct.type === 'otherLiability'
            const isNegative = acct.balance_milliunits < 0
            const balanceColor =
              isCreditLiability && isNegative ? 'var(--color-error)' : 'var(--text-primary)'

            return (
              <Card
                key={acct.name}
                className="px-3 py-2 flex items-center justify-between"
                style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
              >
                <div className="flex-1 min-w-0">
                  <p className="text-[12px] font-medium truncate" style={{ color: 'var(--text-primary)' }}>
                    {acct.name}
                  </p>
                  <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                    {accountTypeLabel(acct.type)}
                  </p>
                </div>
                <span
                  className="text-[12px] font-mono tabular-nums font-semibold flex-shrink-0 ml-2"
                  style={{ color: balanceColor }}
                >
                  {formatCurrency(acct.balance_milliunits)}
                </span>
              </Card>
            )
          })
        )}
      </div>
    </section>
  )
}

// ---------------------------------------------------------------------------
// OverspentList (sidebar)
// ---------------------------------------------------------------------------

function OverspentList({ items }: { items: OverspentCategory[] }) {
  if (items.length === 0) return null

  return (
    <section aria-label="Overspent categories">
      <h2 className="text-[10px] uppercase tracking-wider font-medium mb-2" style={{ color: 'var(--color-error)' }}>
        Overspent ({items.length})
      </h2>
      <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}>
        {items.map((item, i) => (
          <div
            key={`${item.category_group}:${item.category}`}
            className="flex items-center gap-2 px-3 py-2"
            style={{
              borderBottom: i < items.length - 1 ? '1px solid var(--pill-border)' : undefined,
              background: 'color-mix(in srgb, var(--color-error) 5%, transparent)',
            }}
          >
            <span
              className="h-1.5 w-1.5 rounded-full flex-shrink-0"
              style={{ background: 'var(--color-error)' }}
              aria-hidden="true"
            />
            <p className="text-[11px] font-medium truncate flex-1" style={{ color: 'var(--color-error)' }}>
              {item.category}
            </p>
            <span className="text-[11px] font-mono tabular-nums flex-shrink-0" style={{ color: 'var(--color-error)' }}>
              {formatCurrency(item.balance_milliunits)}
            </span>
          </div>
        ))}
      </Card>
    </section>
  )
}

// ---------------------------------------------------------------------------
// YnabView
// ---------------------------------------------------------------------------

export default function YnabView({ isConnected: _isConnected }: PluginViewProps) {
  const [status, setStatus] = useState<YnabStatus | null>(null)
  const [overspent, setOverspent] = useState<OverspentCategory[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadData = useCallback(async () => {
    try {
      const [statusResult, itemsResult] = await Promise.allSettled([
        queryPluginStatus('ynab'),
        queryPluginItems('ynab'),
      ])

      if (statusResult.status === 'fulfilled') {
        setStatus(statusResult.value as unknown as YnabStatus)
        setError(null)
      } else {
        setError(
          statusResult.reason instanceof Error
            ? statusResult.reason.message
            : String(statusResult.reason)
        )
      }

      if (itemsResult.status === 'fulfilled') {
        setOverspent(itemsResult.value as unknown as OverspentCategory[])
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadData()
    const id = setInterval(() => void loadData(), 60_000)
    return () => clearInterval(id)
  }, [loadData])

  if (loading) {
    return (
      <div className="flex flex-col gap-3 p-4">
        <Skeleton className="h-6 w-36" />
        <Skeleton className="h-28 w-full rounded-lg" />
        <Skeleton className="h-5 w-24" />
        {[1, 2, 3].map(i => <Skeleton key={i} className="h-12 w-full rounded-lg" />)}
      </div>
    )
  }

  if (error && !status) {
    return (
      <div className="p-4">
        <Badge variant="destructive" className="mb-2">error</Badge>
        <p className="text-sm font-mono" style={{ color: 'var(--color-error)' }}>{error}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={() => void loadData()}>
          Retry
        </Button>
      </div>
    )
  }

  const categories = status?.categories ?? []
  const transactions = status?.transactions ?? []
  const accounts = status?.accounts ?? []

  return (
    <div className="flex flex-col gap-4 p-4 lg:flex-row lg:items-start">
      {/* Main column */}
      <div className="flex flex-col gap-4 flex-1 min-w-0">
        {status && <BudgetSummaryCard status={status} />}
        {categories.length > 0 && <CategoryAccordion categories={categories} />}
        <TransactionTable
          transactions={transactions}
          categories={categories}
          accounts={accounts}
          onRefresh={() => void loadData()}
        />
      </div>

      {/* Sidebar */}
      <div className="flex flex-col gap-4 lg:w-56 flex-shrink-0">
        <AccountBalanceCards accounts={accounts} />
        {overspent.length > 0 && <OverspentList items={overspent} />}
        <Button
          size="sm"
          variant="outline"
          onClick={() => void loadData()}
          aria-label="Refresh YNAB data"
        >
          Refresh
        </Button>
      </div>
    </div>
  )
}
