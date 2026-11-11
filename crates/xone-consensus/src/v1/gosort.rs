//! geth-XDC `common/sort.Slice` (`common/sort/slice.go:29-190`): the frozen, unstable quicksort
//! that picks the masternodes among equal stakes. The order of equal elements depends on its exact
//! sequence of comparisons and swaps, so it is ported step by step and not replaced by
//! `sort_by`.

struct Data<'a, T, F> {
    v: &'a mut [T],
    less: F,
}

impl<T, F: Fn(&T, &T) -> bool> Data<'_, T, F> {
    fn less(&self, i: usize, j: usize) -> bool {
        (self.less)(&self.v[i], &self.v[j])
    }
    fn swap(&mut self, i: usize, j: usize) {
        self.v.swap(i, j);
    }
}

/// `sort.Slice(x, less)` with geth's frozen algorithm. `less` is called as `less(&x[i], &x[j])`.
pub(crate) fn sort_slice<T, F: Fn(&T, &T) -> bool>(v: &mut [T], less: F) {
    let n = v.len();
    let mut depth = 0usize;
    let mut i = n;
    while i > 0 {
        depth += 1;
        i >>= 1;
    }
    let mut d = Data { v, less };
    quick_sort(&mut d, 0, n, depth * 2);
}

fn quick_sort<T, F: Fn(&T, &T) -> bool>(
    d: &mut Data<'_, T, F>,
    mut a: usize,
    mut b: usize,
    mut max_depth: usize,
) {
    while b - a > 12 {
        if max_depth == 0 {
            heap_sort(d, a, b);
            return;
        }
        max_depth -= 1;
        let (mlo, mhi) = do_pivot(d, a, b);
        if mlo - a < b - mhi {
            quick_sort(d, a, mlo, max_depth);
            a = mhi;
        } else {
            quick_sort(d, mhi, b, max_depth);
            b = mlo;
        }
    }
    if b - a > 1 {
        for i in a + 6..b {
            if d.less(i, i - 6) {
                d.swap(i, i - 6);
            }
        }
        insertion_sort(d, a, b);
    }
}

fn heap_sort<T, F: Fn(&T, &T) -> bool>(d: &mut Data<'_, T, F>, a: usize, b: usize) {
    let (first, hi) = (a, b - a);
    for i in (0..=(hi - 1) / 2).rev() {
        sift_down(d, i, hi, first);
    }
    for i in (0..hi).rev() {
        d.swap(first, first + i);
        sift_down(d, 0, i, first);
    }
}

fn do_pivot<T, F: Fn(&T, &T) -> bool>(
    d: &mut Data<'_, T, F>,
    lo: usize,
    hi: usize,
) -> (usize, usize) {
    let m = (lo + hi) >> 1;
    if hi - lo > 40 {
        let s = (hi - lo) / 8;
        median_of_three(d, lo, lo + s, lo + 2 * s);
        median_of_three(d, m, m - s, m + s);
        median_of_three(d, hi - 1, hi - 1 - s, hi - 1 - 2 * s);
    }
    median_of_three(d, lo, m, hi - 1);
    let pivot = lo;
    let (mut a, mut c) = (lo + 1, hi - 1);
    while a < c && d.less(a, pivot) {
        a += 1;
    }
    let mut b = a;
    loop {
        while b < c && !d.less(pivot, b) {
            b += 1;
        }
        while b < c && d.less(pivot, c - 1) {
            c -= 1;
        }
        if b >= c {
            break;
        }
        d.swap(b, c - 1);
        b += 1;
        c -= 1;
    }
    let mut protect = hi - c < 5;
    if !protect && hi - c < (hi - lo) / 4 {
        let mut dups = 0;
        if !d.less(pivot, hi - 1) {
            d.swap(c, hi - 1);
            c += 1;
            dups += 1;
        }
        if !d.less(b - 1, pivot) {
            b -= 1;
            dups += 1;
        }
        if !d.less(m, pivot) {
            d.swap(m, b - 1);
            b -= 1;
            dups += 1;
        }
        protect = dups > 1;
    }
    if protect {
        loop {
            while a < b && !d.less(b - 1, pivot) {
                b -= 1;
            }
            while a < b && d.less(a, pivot) {
                a += 1;
            }
            if a >= b {
                break;
            }
            d.swap(a, b - 1);
            a += 1;
            b -= 1;
        }
    }
    d.swap(pivot, b - 1);
    (b - 1, c)
}

fn insertion_sort<T, F: Fn(&T, &T) -> bool>(d: &mut Data<'_, T, F>, a: usize, b: usize) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && d.less(j, j - 1) {
            d.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn sift_down<T, F: Fn(&T, &T) -> bool>(d: &mut Data<'_, T, F>, lo: usize, hi: usize, first: usize) {
    let mut root = lo;
    loop {
        let mut child = 2 * root + 1;
        if child >= hi {
            return;
        }
        if child + 1 < hi && d.less(first + child, first + child + 1) {
            child += 1;
        }
        if !d.less(first + root, first + child) {
            return;
        }
        d.swap(first + root, first + child);
        root = child;
    }
}

fn median_of_three<T, F: Fn(&T, &T) -> bool>(
    d: &mut Data<'_, T, F>,
    m1: usize,
    m0: usize,
    m2: usize,
) {
    if d.less(m1, m0) {
        d.swap(m1, m0);
    }
    if d.less(m2, m1) {
        d.swap(m2, m1);
        if d.less(m1, m0) {
            d.swap(m1, m0);
        }
    }
}
