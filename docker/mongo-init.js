const auth = db.getSiblingDB('brnavi_auth');
const password = process.env.AUTH_DB_PASSWORD;
if (!password) {
    throw new Error('AUTH_DB_PASSWORD is required');
}
auth.createUser({
    user: 'brnavi',
    pwd: password,
    roles: [{ role: 'readWrite', db: 'brnavi_auth' }],
});
