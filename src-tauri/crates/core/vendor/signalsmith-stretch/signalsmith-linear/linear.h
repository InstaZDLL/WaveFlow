#ifndef SIGNALSMITH_AUDIO_LINEAR_H
#define SIGNALSMITH_AUDIO_LINEAR_H

#if defined(__FAST_MATH__) && (__apple_build_version__ >= 16000000) && (__apple_build_version__ <= 16000099) && !defined(SIGNALSMITH_IGNORE_BROKEN_APPLECLANG)
#	error Apple Clang 16.0.0 generates incorrect SIMD for ARM. If you HAVE to use this version of Clang, turn off -ffast-math.
#endif

#ifndef M_PI
#	define M_PI 3.14159265358979323846
#endif

#include <cmath>
#include <cassert>
#include <complex>
#include <array>
#include <vector>
#include <type_traits>
#include <functional>

namespace signalsmith { namespace linear {

template<typename V>
using ConstRealPointer = const V *;
template<typename V>
using RealPointer = V *;

template<typename V>
using ConstComplexPointer = const std::complex<V> *;
template<typename V>
using ComplexPointer = std::complex<V> *;

template<typename V>
struct ConstSplitPointer {
	ConstRealPointer<V> real, imag;
	ConstSplitPointer(ConstRealPointer<V> real, ConstRealPointer<V> imag) : real(real), imag(imag) {}

	// Array-like access for convenience
	const std::complex<V> operator[](std::ptrdiff_t i) const {
		return {real[i], imag[i]};
	}
};

template<typename V>
struct SplitPointer {
	RealPointer<V> real, imag;
	SplitPointer(RealPointer<V> real, RealPointer<V> imag) : real(real), imag(imag) {}
	operator ConstSplitPointer<V>() {
		return {real, imag};
	}

	// Array-like access for convenience
	const std::complex<V> operator[](std::ptrdiff_t i) const {
		return {real[i], imag[i]};
	}

	// Assignable (if not const) and converts to `std::complex<V>`
	struct Value : public std::complex<V> {
		Value(V &_real, V &_imag) : std::complex<V>(_real, _imag),  _real(_real), _imag(_imag) {}
	
		V real() const {
			return _real;
		}
		void real(V v) {
			_real = v;
			std::complex<V>::real(v);
		}
		V imag() const {
			return _imag;
		}
		void imag(V v) {
			_imag = v;
			std::complex<V>::imag(v);
		}

#define LINEAR_SPLIT_POINTER_ASSIGNMENT_OP(OP) \
		template<class Other> \
		Value & operator OP(Other &&v) { \
			std::complex<V>::operator OP(std::forward<Other>(v)); \
			_real = v.real(); \
			_imag = v.imag(); \
			return *this; \
		}
		LINEAR_SPLIT_POINTER_ASSIGNMENT_OP(=);
		LINEAR_SPLIT_POINTER_ASSIGNMENT_OP(+=);
		LINEAR_SPLIT_POINTER_ASSIGNMENT_OP(-=);
		LINEAR_SPLIT_POINTER_ASSIGNMENT_OP(*=);
		LINEAR_SPLIT_POINTER_ASSIGNMENT_OP(/=);
#undef LINEAR_SPLIT_POINTER_ASSIGNMENT_OP

	private:
		V &_real;
		V &_imag;
	};

	Value operator[](std::ptrdiff_t i) {
		return {real[i], imag[i]};
	}
};

template<bool=true>
struct LinearImpl;
using Linear = LinearImpl<true>;

// Everything we deal with is actually one of these
template<class BaseExpr>
struct Expression;
template<class BaseExpr>
struct WritableExpression;

#define EXPRESSION_NAME(Class, nameExpr) \
	static std::string name() {\
		return nameExpr; \
	}

//#undef EXPRESSION_NAME
//#include <typeinfo>
//#define EXPRESSION_NAME(Class, nameExpr) \
//	static std::string name() { \
//		return typeid(Class).name(); \
//	}


// Expression templates, which always hold const pointers
namespace expression {
	// All base Exprs inherit from this, so we can SFINAE-test for them
	struct Base {};
	void mustBeExpr(const Base &) {}

	template<typename V>
	struct ConstantExpr : public Base {
		EXPRESSION_NAME(Constant, "V");
		V value;

		ConstantExpr(V value) : value(value) {}
		
		const V get(std::ptrdiff_t) const {
			return value;
		}
	};

	template<class V, typename=void>
	struct ExprTest {
		using Constant = ConstantExpr<V>;

		static Constant wrap(const V &v) {
			return {v};
		}
	};
	template<class Expr>
	struct ExprTest<Expr, decltype(mustBeExpr(std::declval<Expr>()))> {
		static Expr wrap(const Expr &expr) {
			return expr;
		}
	};
	// Constant class, only defined for non-Expr types
	template<class Expr>
	using Constant = typename ExprTest<Expr>::Constant;
	
	template<class Expr>
	auto ensureExpr(const Expr &expr) -> decltype(ExprTest<Expr>::wrap(expr)){
		return ExprTest<Expr>::wrap(expr);
	};

	// Expressions that just read from a pointer
	template<typename V>
	struct ReadableReal : public Base {
		EXPRESSION_NAME(ReadableReal, "const V*");
		ConstRealPointer<V> pointer;

		ReadableReal(ConstRealPointer<V> pointer) : pointer(pointer) {}
		
		V get(std::ptrdiff_t i) const {
			return pointer[i];
		}
	};
	template<typename V>
	struct ReadableComplex : public Base {
		EXPRESSION_NAME(ReadableComplex, "const VC*");
		ConstComplexPointer<V> pointer;

		ReadableComplex(ConstComplexPointer<V> pointer) : pointer(pointer) {}

		std::complex<V> get(std::ptrdiff_t i) const {
			return pointer[i];
		}
	};
	template<typename V>
	struct ReadableSplit : public Base {
		EXPRESSION_NAME(ReadableSplit, "const VS*");
		ConstSplitPointer<V> pointer;

		ReadableSplit(ConstSplitPointer<V> pointer) : pointer(pointer) {}

		std::complex<V> get(std::ptrdiff_t i) const {
			return {pointer.real[i], pointer.imag[i]};
		}
	};
}
	
// + - * / % ^ & | ~ ! = < > += -= *= /= %= ^= &= |= << >> >>= <<= == != <= >= <=>(since C++20) && || ++ -- , ->* -> ( ) [ ]

#define SIGNALSMITH_AUDIO_LINEAR_UNARY_PREFIX(Name, OP) \
namespace expression { \
	template<class A> \
	struct Name : public Base { \
		EXPRESSION_NAME(Name, (#Name "<") + A::name() + ">"); \
		A a; \
		Name(const A &a) : a(a) {} \
		auto get(std::ptrdiff_t i) const -> decltype(OP a.get(i)) const { \
			return OP a.get(i); \
		} \
	}; \
	template<class A> \
	Name<A> make##Name(A a) { \
		return {a}; \
	} \
} \
template<class A> \
Expression<expression::Name<A>> operator OP(const Expression<A> &a) { \
	return {a}; \
}
SIGNALSMITH_AUDIO_LINEAR_UNARY_PREFIX(Neg, -)
// Two negatives cancel out
template<class A>
Expression<A> operator-(const Expression<expression::Neg<A>> &expr) { \
	return expr.a;
}
#undef SIGNALSMITH_AUDIO_LINEAR_UNARY_PREFIX

#define SIGNALSMITH_AUDIO_LINEAR_BINARY_INFIX(Name, OP) \
namespace expression { \
	template<class A, class B> \
	struct Name : public Base { \
		EXPRESSION_NAME(Name, (#Name "<") + A::name() + "," + B::name() + ">"); \
		A a; \
		B b; \
		Name(const A &a, const B &b) : a(a), b(b) {} \
		auto get(std::ptrdiff_t i) const -> decltype(a.get(i) OP b.get(i)) const { \
			return a.get(i) OP b.get(i); \
		} \
	}; \
	template<class A, class B> \
	Name<A, B> make##Name(A a, B b) { \
		return {a, b}; \
	} \
} \
template<class A, class B> \
const Expression<expression::Name<A, B>> operator OP(const Expression<A> &a, const Expression<B> &b) { \
	return {a, b}; \
} \
template<class A, class B> \
const Expression<expression::Name<A, expression::Constant<B>>> operator OP(const Expression<A> &a, const B &b) { \
	return {a, b}; \
} \
template<class A, class B> \
const Expression<expression::Name<expression::Constant<A>, B>> operator OP(const A &a, const Expression<B> &b) { \
	return {a, b}; \
}
SIGNALSMITH_AUDIO_LINEAR_BINARY_INFIX(Add, +)
SIGNALSMITH_AUDIO_LINEAR_BINARY_INFIX(Mul, *)
SIGNALSMITH_AUDIO_LINEAR_BINARY_INFIX(Sub, -)
SIGNALSMITH_AUDIO_LINEAR_BINARY_INFIX(Div, /)
#undef SIGNALSMITH_AUDIO_LINEAR_BINARY_INFIX

namespace expression {
#define SIGNALSMITH_AUDIO_LINEAR_FUNC1(Name, func) \
	template<class A> \
	struct Name; \
	template<class A> \
	Name<A> make##Name(A a) { \
		return {a}; \
	} \
	template<class A> \
	struct Name : public Base { \
		EXPRESSION_NAME(Name, (#Name "<") + A::name() + ">"); \
		A a; \
		Name(const A &a) : a(a) {} \
		auto get(std::ptrdiff_t i) const -> decltype(func(a.get(i))) { \
			return func(a.get(i)); \
		} \
	};

	template<class A>
	A fastAbs(const A &a) {
		return std::abs(a);
	}
	template<class A>
	A fastAbs(const std::complex<A> &a) {
		return std::hypot(a.real(), a.imag());
	}
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Abs, fastAbs)
	
	template<class A>
	A fastNorm(const A &a) {
		return a*a;
	}
	template<class A>
	A fastNorm(const std::complex<A> &a) {
		A real = a.real(), imag = a.imag();
		return real*real + imag*imag;
	}
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Norm, fastNorm)

	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Exp, std::exp)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Exp2, std::exp2)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Log, std::log)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Log2, std::log2)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Log10, std::log10)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Sqrt, std::sqrt)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Cbrt, std::cbrt)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Floor, std::floor)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Ceil, std::ceil)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Conj, std::conj)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Real, std::real)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Imag, std::imag)
	SIGNALSMITH_AUDIO_LINEAR_FUNC1(Arg, std::arg)
#undef SIGNALSMITH_AUDIO_LINEAR_FUNC1
} // expression::

template<class BaseExpr>
struct Expression : public BaseExpr {
	template<class ...Args>
	Expression(Args &&...args) : BaseExpr(std::forward<Args>(args)...) {
		static_assert(std::is_trivially_copyable<Expression>::value, "Expression<> must be trivially copyable");
		static_assert(std::is_trivially_copyable<BaseExpr>::value, "BaseExpr must be trivially copyable");
	}

	auto operator[](std::ptrdiff_t i) -> decltype(BaseExpr::get(i)) const {
		return BaseExpr::get(i);
	}

	const Expression<expression::Abs<BaseExpr>> abs() const {
		return {*this};
	}
	const Expression<expression::Norm<BaseExpr>> norm() const {
		return {*this};
	}
	const Expression<expression::Exp<BaseExpr>> exp() const {
		return {*this};
	}
	const Expression<expression::Exp2<BaseExpr>> exp2() const {
		return {*this};
	}
	const Expression<expression::Log<BaseExpr>> log() const {
		return {*this};
	}
	const Expression<expression::Log2<BaseExpr>> log2() const {
		return {*this};
	}
	const Expression<expression::Log10<BaseExpr>> log10() const {
		return {*this};
	}
	const Expression<expression::Sqrt<BaseExpr>> sqrt() const {
		return {*this};
	}
	const Expression<expression::Cbrt<BaseExpr>> cbrt() const {
		return {*this};
	}
	const Expression<expression::Conj<BaseExpr>> conj() const {
		return {*this};
	}
	const Expression<expression::Real<BaseExpr>> real() const {
		return {*this};
	}
	const Expression<expression::Imag<BaseExpr>> imag() const {
		return {*this};
	}
	const Expression<expression::Arg<BaseExpr>> arg() const {
		return {*this};
	}
	const Expression<expression::Floor<BaseExpr>> floor() const {
		return {*this};
	}
	const Expression<expression::Ceil<BaseExpr>> ceil() const {
		return {*this};
	}
};
template<class BaseExpr>
struct WritableExpression : public Expression<BaseExpr> {
	using Expression<BaseExpr>::Expression;

	WritableExpression(const WritableExpression &other) = default;

	template<class Expr>
	WritableExpression & operator=(const Expr &expr) {
		this->linear.fill(this->pointer, expression::ensureExpr(expr), this->size);
		return *this;
	}

	WritableExpression & operator=(const WritableExpression &expr) {
		this->linear.fill(this->pointer, expr, this->size);
		return *this;
	}

	// Use the pointer's `operator[]` instead of the expression
	auto operator[](std::ptrdiff_t i) -> decltype(this->pointer[i]) {
		return this->pointer[i];
	}

	auto operator[](std::ptrdiff_t i) const -> decltype(this->pointer[i]) {
		return this->pointer[i];
	}
};

/// Helper class for temporary storage
template<typename V, size_t alignBytes=sizeof(V)>
struct Temporary {
	// This is called if we don't have enough reserved space and end up allocating
	std::function<void(size_t)> allocationWarning;
	
	void reserve(size_t size) {
		if (buffer) delete[] buffer;
		buffer = new V[size];
		alignedBuffer = nextAligned(buffer);
		if (alignedBuffer != buffer) {
			delete[] buffer;
			buffer = new V[size + extraAlignmentItems];
			alignedBuffer = nextAligned(buffer);
		}
		start = alignedBuffer;
		end = alignedBuffer + size;
	}

	void clear() {
		start = alignedBuffer;
		fallbacks.resize(0);
		fallbacks.shrink_to_fit();
	}

	// You should have one of these every time you're using the temporary storage
	struct Scoped {
		Scoped(Temporary &temporary) : temporary(temporary), restoreStart(temporary.start), fallbackSize(temporary.fallbacks.size()) {}
		
		~Scoped() {
			temporary.start = restoreStart;
			temporary.fallbacks.resize(fallbackSize);
		}
		
		V * operator()(size_t size) {
			return temporary.getChunk(size);
		}

	private:
		Temporary &temporary;
		V *restoreStart;
		size_t fallbackSize;
	};

private:
	static constexpr size_t extraAlignmentItems = alignBytes/sizeof(V);
	static V * nextAligned(V *ptr) {
		return (V*)((size_t(ptr) + (alignBytes - 1))&~(alignBytes - 1));
	}

	size_t depth = 0;
	V *start = nullptr, *end = nullptr;
	V *buffer = nullptr, *alignedBuffer = nullptr;

	std::vector<std::vector<V>> fallbacks;

	/// Valid until the next call to .clear() or .reserve()
	V * getChunk(size_t size) {
		V *result = start;
		V *newStart = start + size;
		if (newStart > end) {
			// OK, actually we ran out of temporary space, so allocate
			fallbacks.emplace_back(size + extraAlignmentItems);
			result = nextAligned(fallbacks.back().data());
			// but we're not happy about it. >:(
			if (allocationWarning) allocationWarning(newStart - buffer);
		}
		start = nextAligned(newStart);
		return result;
	}
};

template<class Linear, size_t alignBytes=1>
struct CachedResults {
	using TemporaryFloats = Temporary<float, alignBytes>;
	using TemporaryDoubles = Temporary<double, alignBytes>;

	template<typename V>
	using WritableReal = typename Linear::template WritableReal<V>;
	template<typename V>
	using WritableComplex = typename Linear::template WritableComplex<V>;
	template<typename V>
	using WritableSplit = typename Linear::template WritableSplit<V>;
	
	CachedResults(Linear &linear) : linear(linear) {}
	
	struct ScopedFloat : public TemporaryFloats::Scoped {
		ScopedFloat(Linear &linear, TemporaryFloats &temporary) : TemporaryFloats::Scoped(temporary), linear(linear) {}
		
		template<class Expr>
		ConstRealPointer<float> real(Expr expr, size_t size) {
			auto chunk = (*this)(size);
			linear.fill(chunk, expr, size);
			return chunk;
		}
		ConstRealPointer<float> real(expression::ReadableReal<float> expr, size_t) {
			return expr.pointer;
		}
		ConstRealPointer<float> real(WritableReal<float> expr, size_t) {
			return expr.pointer;
		}
		template<class Expr>
		ConstRealPointer<float> real(Expr expr, size_t size, RealPointer<float> canUse) {
			linear.fill(canUse, expr, size);
			return canUse;
		}
		ConstRealPointer<float> real(expression::ReadableReal<float> expr, size_t, RealPointer<float>) {
			return expr.pointer;
		}
		ConstRealPointer<float> real(WritableReal<float> expr, size_t, RealPointer<float>) {
			return expr.pointer;
		}
		template<class Expr>
		ConstComplexPointer<float> complex(Expr expr, size_t size) {
			auto chunk = (*this)(size*2);
			linear.fill((ComplexPointer<float>)chunk, expr, size);
			return chunk;
		}
		ConstComplexPointer<float> complex(expression::ReadableComplex<float> expr, size_t) {
			return expr.pointer;
		}
		ConstComplexPointer<float> complex(WritableComplex<float> expr, size_t) {
			return expr.pointer;
		}
		template<class Expr>
		ConstComplexPointer<float> complex(Expr expr, size_t size, ComplexPointer<float> canUse) {
			linear.fill(canUse, expr, size);
			return canUse;
		}
		ConstComplexPointer<float> complex(expression::ReadableComplex<float> expr, size_t, ComplexPointer<float>) {
			return expr.pointer;
		}
		ConstComplexPointer<float> complex(WritableComplex<float> expr, size_t, ComplexPointer<float>) {
			return expr.pointer;
		}
		template<class Expr>
		ConstSplitPointer<float> split(Expr expr, size_t size) {
			SplitPointer<float> chunk{(*this)(size), (*this)(size)};
			linear.fill(chunk, expr, size);
			return chunk;
		}
		ConstSplitPointer<float> split(expression::ReadableSplit<float> expr, size_t) {
			return expr.pointer;
		}
		ConstSplitPointer<float> split(WritableSplit<float> expr, size_t) {
			return expr.pointer;
		}
		template<class Expr>
		ConstSplitPointer<float> split(Expr expr, size_t size, SplitPointer<float> canUse) {
			linear.fill(canUse, expr, size);
			return canUse;
		}
		ConstSplitPointer<float> split(expression::ReadableSplit<float> expr, size_t, SplitPointer<float>) {
			return expr.pointer;
		}
		ConstSplitPointer<float> split(WritableSplit<float> expr, size_t, SplitPointer<float>) {
			return expr.pointer;
		}
	private:
		Linear &linear;
	};
	ScopedFloat floatScope() {
		return {linear, floats};
	}
	void reserveFloats(size_t size) {
		floats.reserve(size);
	}
	template<class Fn>
	void reserveFloats(size_t size, Fn &&allocationWarning) {
		floats.reserve(size);
		floats.allocationWarning = allocationWarning;
	}

	struct ScopedDouble : public TemporaryDoubles::Scoped {
		ScopedDouble(Linear &linear, TemporaryDoubles &temporary) : TemporaryDoubles::Scoped(temporary), linear(linear) {}
		
		template<class Expr>
		ConstRealPointer<double> real(Expr expr, size_t size) {
			auto chunk = (*this)(size);
			linear.fill(chunk, expr, size);
			return chunk;
		}
		ConstRealPointer<double> real(expression::ReadableReal<double> expr, size_t) {
			return expr.pointer;
		}
		ConstRealPointer<double> real(WritableReal<double> expr, size_t) {
			return expr.pointer;
		}
		template<class Expr>
		ConstRealPointer<double> real(Expr expr, size_t size, RealPointer<double> canUse) {
			linear.fill(canUse, expr, size);
			return canUse;
		}
		ConstRealPointer<double> real(expression::ReadableReal<double> expr, size_t, RealPointer<double>) {
			return expr.pointer;
		}
		ConstRealPointer<double> real(WritableReal<double> expr, size_t, RealPointer<double>) {
			return expr.pointer;
		}
		template<class Expr>
		ConstComplexPointer<double> complex(Expr expr, size_t size) {
			auto chunk = (*this)(size*2);
			linear.fill((ComplexPointer<double>)chunk, expr, size);
			return chunk;
		}
		ConstComplexPointer<double> complex(expression::ReadableComplex<double> expr, size_t) {
			return expr.pointer;
		}
		ConstComplexPointer<double> complex(WritableComplex<double> expr, size_t) {
			return expr.pointer;
		}
		template<class Expr>
		ConstComplexPointer<double> complex(Expr expr, size_t size, ComplexPointer<double> canUse) {
			linear.fill(canUse, expr, size);
			return canUse;
		}
		ConstComplexPointer<double> complex(expression::ReadableComplex<double> expr, size_t, ComplexPointer<double>) {
			return expr.pointer;
		}
		ConstComplexPointer<double> complex(WritableComplex<double> expr, size_t, ComplexPointer<double>) {
			return expr.pointer;
		}
		template<class Expr>
		ConstSplitPointer<double> split(Expr expr, size_t size) {
			SplitPointer<double> chunk{(*this)(size), (*this)(size)};
			linear.fill(chunk, expr, size);
			return chunk;
		}
		ConstSplitPointer<double> split(expression::ReadableSplit<double> expr, size_t) {
			return expr.pointer;
		}
		ConstSplitPointer<double> split(WritableSplit<double> expr, size_t) {
			return expr.pointer;
		}
		template<class Expr>
		ConstSplitPointer<double> split(Expr expr, size_t size, SplitPointer<double> canUse) {
			linear.fill(canUse, expr, size);
			return canUse;
		}
		ConstSplitPointer<double> split(expression::ReadableSplit<double> expr, size_t, SplitPointer<double>) {
			return expr.pointer;
		}
		ConstSplitPointer<double> split(WritableSplit<double> expr, size_t, SplitPointer<double>) {
			return expr.pointer;
		}
	private:
		Linear &linear;
	};
	ScopedDouble doubleScope() {
		return {linear, doubles};
	}
	void reserveDoubles(size_t size) {
		doubles.reserve(size);
	}
	template<class Fn>
	void reserveDoubles(size_t size, Fn &&allocationWarning) {
		doubles.reserve(size);
		doubles.allocationWarning = allocationWarning;
	}
	
private:
	TemporaryFloats floats;
	TemporaryDoubles doubles;
	Linear &linear;
};

template<bool useLinear=true>
struct LinearImplBase {
	using Linear = LinearImpl<useLinear>;

	template<class V>
	void reserve(size_t) {}

	template<typename V>
	struct WritableReal {
		EXPRESSION_NAME(WritableReal, "V*");
		Linear &linear;
		RealPointer<V> pointer;
		size_t size;
		WritableReal(Linear &linear, RealPointer<V> pointer, size_t size) : linear(linear), pointer(pointer), size(size) {
			static_assert(std::is_trivially_copyable<WritableReal>::value, "must be trivially copyable");
		}
		
		operator expression::ReadableReal<V>() const {
			return {pointer};
		}
		
		V get(std::ptrdiff_t i) const {
			return pointer[i];
		}

		V & operator[](std::ptrdiff_t i) {
			return pointer[i];
		}
		const V & operator[](std::ptrdiff_t i) const {
			return pointer[i];
		}
	};
	template<typename V>
	struct WritableComplex {
		EXPRESSION_NAME(WritableComplex, "VC*");
		Linear &linear;
		ComplexPointer<V> pointer;
		size_t size;
		WritableComplex(Linear &linear, ComplexPointer<V> pointer, size_t size) : linear(linear), pointer(pointer), size(size) {}

		operator expression::ReadableComplex<V>() const {
			return {pointer};
		}

		std::complex<V> get(std::ptrdiff_t i) const {
			return pointer[i];
		}

		std::complex<V> & operator[](std::ptrdiff_t i) {
			return pointer[i];
		}
		const std::complex<V> & operator[](std::ptrdiff_t i) const {
			return pointer[i];
		}
	};
	template<typename V>
	struct WritableSplit {
		EXPRESSION_NAME(WritableSplit, "VS*");
		Linear &linear;
		SplitPointer<V> pointer;
		size_t size;
		WritableSplit(Linear &linear, SplitPointer<V> pointer, size_t size) : linear(linear), pointer(pointer), size(size) {}

		operator expression::ReadableSplit<V>() const {
			return {pointer};
		}

		std::complex<V> get(std::ptrdiff_t i) const {
			return {pointer.real[i], pointer.imag[i]};
		}
		auto operator[](std::ptrdiff_t i) -> decltype(pointer[i]) {
			return pointer[i];
		}
	};

	// Wrap a pointer as an expression
	template<typename V>
	Expression<expression::ReadableReal<V>> wrap(ConstRealPointer<V> pointer) {
		return {pointer};
	}
	template<typename V>
	Expression<expression::ReadableComplex<V>> wrap(ConstComplexPointer<V> pointer) {
		return {pointer};
	}
	template<typename V>
	Expression<expression::ReadableSplit<V>> wrap(ConstSplitPointer<V> pointer) {
		return {pointer};
	}

	// When a length is supplied, make it writable
	template<typename V>
	WritableExpression<WritableReal<V>> wrap(RealPointer<V> pointer, size_t size) {
		return {self(), pointer, size};
	}
	template<typename V>
	WritableExpression<WritableComplex<V>> wrap(ComplexPointer<V> pointer, size_t size) {
		return {self(), pointer, size};
	}
	template<typename V>
	WritableExpression<WritableSplit<V>> wrap(SplitPointer<V> pointer, size_t size) {
		return {self(), pointer, size};
	}

	template<typename V>
	WritableExpression<WritableReal<V>> wrap(std::vector<V> &vector) {
		return {self(), vector.data(), vector.size()};
	}
	template<typename V>
	WritableExpression<WritableComplex<V>> wrap(std::vector<std::complex<V>> &vector) {
		return {self(), vector.data(), vector.size()};
	}
	template<typename V>
	WritableExpression<WritableSplit<V>> wrap(std::vector<V> &real, std::vector<V> &imag) {
		SplitPointer<V> pointer{real.data(), imag.data()};
		size_t size = std::min<size_t>(real.size(), imag.size());
		return {self(), pointer, size};
	}

	template<typename V>
	Expression<expression::ReadableReal<V>> wrap(const std::vector<V> &vector) {
		return {vector.data()};
	}
	template<typename V>
	Expression<expression::ReadableComplex<V>> wrap(const std::vector<std::complex<V>> &vector) {
		return {vector.data()};
	}
	template<typename V>
	Expression<expression::ReadableSplit<V>> wrap(const std::vector<V> &real, const std::vector<V> &imag) {
		ConstSplitPointer<V> pointer{real.data(), imag.data()};
		return {pointer};
	}

	template<class ...Args>
	auto operator()(Args &&...args) -> decltype(wrap(std::forward<Args>(args)...)) {
		return wrap(std::forward<Args>(args)...);
	}

	template<class Pointer, class Expr>
	void fill(Pointer pointer, Expr expr, size_t size) {
		for (size_t i = 0; i < size; ++i) {
			pointer[i] = expr.get(i);
		}
	}
	template<class V, class Expr>
	void fill(SplitPointer<V> pointer, Expr expr, size_t size) {
		for (size_t i = 0; i < size; ++i) {
			std::complex<V> v = expr.get(i);
			pointer.real[i] = v.real();
			pointer.imag[i] = v.imag();
		}
	}

	// Remove the Expression<...> layer, so the simplification template-matching works
	template<class Pointer, class Expr>
	void fill(Pointer pointer, Expression<Expr> expr, size_t size) {
		return self().fill(pointer, (Expr &)expr, size);
	};
	// Separate otherwise it's ambiguous between the two previous ones
	template<class V, class Expr>
	void fill(SplitPointer<V> pointer, Expression<Expr> expr, size_t size) {
		return self().fill(pointer, (Expr &)expr, size);
	};

protected:
	LinearImplBase(Linear *linearThis) {
		assert((LinearImplBase *)linearThis == this);
	}

	Linear & self() {
		return *(Linear *)this;
	}
};

/* SFINAE template for checking that an expression naturally returns a particular item type

e.g.
	template<class A>
	ItemType<A, float, void> fill(...)
*/
template<class InputExpr, typename Item, class OutputExpr>
using ItemType = typename std::enable_if<
	std::is_same<
		typename std::decay<decltype(std::declval<InputExpr>().get(0))>::type,
		Item
	>::value,
	OutputExpr
>::type;

// Fallback implementation - this should be specialised (with useLinear=true) with faster methods where available
template<bool useLinear>
struct LinearImpl : public LinearImplBase<useLinear> {
	LinearImpl() : LinearImplBase<useLinear>(this), cached(*this) {}

	using LinearImplBase<useLinear>::fill;
	
	// Override .fill() for specific pointer/expressions which you can do quickly.  Calling `cached.realFloat()` etc. will call back to .fill()
	template<class Expr>
	void fill(RealPointer<float> pointer, expression::Sqrt<expression::Norm<Expr>> expr, size_t size) {
		auto floats = cached.floatScope();
		
		auto normExpr = expr.a;
		auto array = floats.real(normExpr, size);
		
		// The idea is to use an existing fast function, but this is an example
		for (size_t i = 0; i < size; ++i) {
			pointer[i] = std::sqrt(array[i]);
		}
	}
	
	template<typename V>
	void reserve(size_t) {}
	// This makes sure we don't allocate - unless there's an expression with multiple sqrts in it!
	template<>
	void reserve<float>(size_t size) {
		cached.reserveFloats(size);
	}
private:
	CachedResults<LinearImpl, 32> cached;
};

}}; // namespace

#if defined(SIGNALSMITH_USE_ACCELERATE)
#	include "./platform/linear-accelerate.h"
#elif 0//defined(SIGNALSMITH_USE_IPP)
#	include "./platform/linear-ipp.h"
#elif 0//defined(SIGNALSMITH_USE_CBLAS)
#	include "./platform/linear-cblas.h"
#endif

#undef SIGNALSMITH_AUDIO_LINEAR_CHUNK_SIZE
#undef SIGNALSMITH_AUDIO_LINEAR_CHUNK_FOREACH_STEP
#undef SIGNALSMITH_AUDIO_LINEAR_CHUNK_FOREACH

#endif // include guard
